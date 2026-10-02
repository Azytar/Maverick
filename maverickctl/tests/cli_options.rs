//! Tests for option scope: which flags this tool consumes, and which words
//! reach the window manager as an action.
//!
//! The control protocol is line-based and the window manager owns the action
//! grammar, so an option that leaks into the action line is not a formatting
//! mistake — it is a *different action*, refused by the instance. The fixture
//! below records what actually arrived on the socket, so these assert the wire
//! and not a reconstruction of it.

use maverick_sys::identity::InstanceInfo;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;

mod runtime_dir;

/// This binary's private runtime directory, so the tool under test resolves a
/// session this binary published rather than whatever is live on the machine.
fn runtime_dir() -> PathBuf {
    runtime_dir::isolate("maverick-cli-options")
}

/// An instance that records every request line and answers `ok`.
///
/// The recorded line is the contract under test: it is what the window manager
/// would execute. Answering `ok` (rather than a refusal) keeps the exit status
/// out of it, so a failure here can only mean the wrong bytes were sent.
///
/// The returned handle owns the listener, its thread and the identity record it
/// published, and takes all three back when dropped. That is what makes the
/// fixture a fixture: without it, the socket stays bound by a thread nothing
/// can reach, the record stays on disk reading `alive`, and a sibling test that
/// resolves a session by context finds this instance instead of none — which
/// answers with an error about *this* session rather than about the one the
/// command asked for.
struct Recording {
    sid: String,
    stop: Option<mpsc::Sender<()>>,
    worker: Option<JoinHandle<()>>,
    recorded: Receiver<String>,
}

impl Recording {
    /// The next request line the instance received, or `None` if none arrives
    /// within `timeout`.
    fn next_line(&self, timeout: std::time::Duration) -> Option<String> {
        self.recorded.recv_timeout(timeout).ok()
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        // The signal is a channel, not a connection to the fixture's own
        // socket: waking a thread blocked in `accept` by connecting to it works
        // only while the socket file is there, and a fixture whose teardown runs
        // after something else has unlinked it would then join a thread that can
        // never be woken.
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        // Joining before the unlink is what makes the cleanup deterministic
        // rather than a race with a thread that is on its way out.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        // Only this session's own socket and record. `cleanup_meta` unlinks by
        // type, so it cannot follow a path that is not the fixture's.
        maverick_sys::identity::cleanup_meta(&self.sid);
    }
}

fn serve_recording(sid: &str) -> Recording {
    let sid = sid.to_string();
    let path = maverick_sys::identity::sock_path(&sid);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create fixture session dir");
    }
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).expect("bind fixture socket");
    maverick_sys::identity::write_meta(&InstanceInfo {
        name: sid.clone(),
        session_id: sid.clone(),
        pid: std::process::id(),
        display: String::new(),
        tty_nr: 0,
        x_server_identity: String::new(),
        start_time: 0,
        exe: String::new(),
        started_at: 0,
        alive: true,
    })
    .expect("write fixture ficha");

    let (tx, recorded) = mpsc::channel();
    let (stop, stopped) = mpsc::channel::<()>();
    let name = sid.clone();
    // Non-blocking accept, waiting on the stop channel in between: `accept` has
    // no timeout, so without this the thread could only be woken by a
    // connection, and a fixture nobody connects to could never be joined.
    listener
        .set_nonblocking(true)
        .expect("make the fixture listener cancellable");
    let worker = std::thread::spawn(move || loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let mut reader = BufReader::new(&stream);
                let mut request = String::new();
                if reader.read_line(&mut request).is_err() {
                    continue;
                }
                let request = request.trim_end().to_string();
                let _ = tx.send(request.clone());
                let reply = if request == "ping" {
                    format!("pong {name}\n")
                } else {
                    "ok\n".to_string()
                };
                let _ = stream.write_all(reply.as_bytes());
                let _ = stream.flush();
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Nothing to serve. This wait is what makes teardown possible;
                // its length bounds how long a drop takes, not what any test
                // observes.
                if stopped
                    .recv_timeout(std::time::Duration::from_millis(1))
                    .is_ok()
                {
                    break;
                }
            }
            Err(_) => break,
        }
    });
    Recording {
        sid,
        stop: Some(stop),
        worker: Some(worker),
        recorded,
    }
}

/// Run `maverickctl` and return the action line the instance received.
///
/// The fixture records *every* request, and instance resolution may open with a
/// `ping` before the command itself — so the recorded line is the `dispatch`
/// this command produced, not simply the first line seen. Reading only the first
/// would make the result depend on whether the resolution ping happened to
/// arrive first.
fn dispatched(args: &[&str]) -> String {
    runtime_dir();
    let sid = "clioptions";
    // Held until the line is read: the instance has to be listening while the
    // command runs, and dropped with it so the next command starts against a
    // clean namespace rather than one this fixture is still occupying.
    let server = serve_recording(sid);
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_maverickctl"));
    cmd.args(args).arg("--session").arg(sid);
    let out = cmd.output().expect("run maverickctl");
    assert!(
        out.status.success(),
        "`maverickctl {args:?}` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let Some(line) = server.next_line(deadline - std::time::Instant::now()) else {
            break;
        };
        if line.starts_with("dispatch") {
            return line;
        }
    }
    panic!("`maverickctl {args:?}` put no dispatch on the wire")
}

/// A dropped recording server takes its socket, its record and its thread with
/// it, and leaves nothing behind a sibling could resolve.
///
/// The alternative is invisible from the test that made the fixture: the socket
/// stays bound by a thread nothing can reach and the record stays on disk
/// reading `alive`, so `maverickctl` invoked without a session resolves *this*
/// instance and reports an error about it. The tests that expect a command to
/// fail for want of a named session would then be asserting against the wrong
/// error.
#[test]
fn a_dropped_recording_server_leaves_nothing_discoverable() {
    runtime_dir();
    let sid = "clileak";
    let sock = maverick_sys::identity::sock_path(sid);
    let meta = maverick_sys::identity::try_meta_path(sid).expect("a valid session id");
    assert!(
        maverick_sys::identity::read_meta(sid).is_none(),
        "the fixture's namespace must start empty, or this proves nothing"
    );

    {
        let server = serve_recording(sid);
        assert!(
            sock.exists(),
            "the fixture must publish a socket to be leaked"
        );
        assert!(
            maverick_sys::identity::read_meta(sid).is_some(),
            "the fixture must publish a record to be leaked"
        );
        // It has to be reachable while alive, or the drop could "clean up" a
        // server that never worked.
        assert!(
            std::os::unix::net::UnixStream::connect(&sock).is_ok(),
            "the fixture must accept a connection while it is held"
        );
        drop(server);
    }

    assert!(!sock.exists(), "the socket outlived its owner: {sock:?}");
    assert!(
        !meta.exists(),
        "the identity record outlived its owner: {meta:?}"
    );
    assert!(
        maverick_sys::identity::read_meta(sid).is_none(),
        "the session is still resolvable after the fixture was dropped"
    );
}

/// A teardown must not reach past its own session.
///
/// The fixture publishes under one session id, and the tests in this binary
/// share a runtime directory. `cleanup_meta` unlinks by type under the session
/// directory it is given, so a fixture that called it with a different id would
/// delete a neighbour's socket — which is how one test ends up failing because
/// another one tidied up.
#[test]
fn a_dropped_recording_server_spares_the_others() {
    runtime_dir();
    let (mine, theirs) = ("clispare-mine", "clispare-theirs");

    let other = serve_recording(theirs);
    let sock = maverick_sys::identity::sock_path(theirs);
    assert!(sock.exists(), "the neighbour must be up to be spared");

    {
        let _own = serve_recording(mine);
        // `_own` drops here, while `other` is still held.
    }

    assert!(
        sock.exists(),
        "dropping one fixture removed a session it never created: {sock:?}"
    );
    assert!(
        maverick_sys::identity::read_meta(theirs).is_some(),
        "dropping one fixture removed another session's record"
    );
    drop(other);
}

/// Repeated creation and teardown is what the option tests do, and each round has
/// to end clean or the next one starts against a leftover.
#[test]
fn a_recording_server_can_be_created_and_dropped_repeatedly() {
    runtime_dir();
    let sid = "clicycle";
    for round in 0..3 {
        let server = serve_recording(sid);
        let sock = maverick_sys::identity::sock_path(sid);
        assert!(
            sock.exists(),
            "round {round}: the fixture must publish a socket"
        );
        assert!(
            std::os::unix::net::UnixStream::connect(&sock).is_ok(),
            "round {round}: the fixture must accept while held"
        );
        drop(server);
        assert!(
            !sock.exists(),
            "round {round}: the socket outlived its owner"
        );
        assert!(
            maverick_sys::identity::read_meta(sid).is_none(),
            "round {round}: the record outlived its owner"
        );
    }
}

/// A formatting option is this tool's, so it must not become action text.
///
/// `msg` and the verbatim-forward path join their positionals into the action
/// line. `--json` was not recognised by that parser, so `msg focus-left --json`
/// put the literal action `focus-left --json` on the wire — a *different*
/// action, which the window manager's grammar refuses. The three
/// session-aware parsers consumed it; these two did not.
#[test]
fn a_formatting_option_never_reaches_the_action_line() {
    for (args, expected) in [
        (vec!["msg", "focus-left"], "dispatch focus-left"),
        (vec!["msg", "focus-left", "--json"], "dispatch focus-left"),
        (vec!["msg", "-j", "focus-left"], "dispatch focus-left"),
        (vec!["msg", "--json", "focus-left"], "dispatch focus-left"),
        // Multiword actions keep their argument boundaries.
        (vec!["msg", "view", "3", "--json"], "dispatch view 3"),
        (
            vec!["msg", "move_window", "right", "0x42003"],
            "dispatch move_window right 0x42003",
        ),
        // The verbatim-forward path, which had the same gap.
        (vec!["focus-left", "--json"], "dispatch focus-left"),
        (vec!["view", "3"], "dispatch view 3"),
    ] {
        assert_eq!(
            dispatched(&args),
            expected,
            "`maverickctl {args:?}` put the wrong line on the wire"
        );
    }
}

/// `--json` still selects JSON output where a document is produced.
///
/// `state` always prints a JSON snapshot, so the flag is a no-op there; the
/// point is that accepting it does not change what is printed, and that stdout
/// carries the document alone.
#[test]
fn json_output_stays_a_single_document_on_stdout() {
    runtime_dir();
    let sid = "clijson";
    // Held for the command: `state` asks the instance for the snapshot, and the
    // fixture answers `ok`, which is what the assertions below read.
    let _server = serve_recording(sid);
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_maverickctl"))
        .args(["state", "--json"])
        .arg("--session")
        .arg(sid)
        .output()
        .expect("run maverickctl");
    // The fixture answers `ok`, which is not JSON: the tool printed it verbatim
    // rather than wrapping or annotating it, and stdout stayed a single line.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("--json"),
        "the flag itself must not appear in the output: {stdout}"
    );
    assert_eq!(
        stdout.lines().count(),
        1,
        "stdout must carry one document, not the flag and a document: {stdout}"
    );
}

/// An option's *value* is not the session name.
///
/// `session_target` used to take the first argument that did not start with a
/// dash. That cannot tell a value from a name: `logs -n 5` asks for five lines,
/// and the scan resolved the session to `"5"`, so the tool reported a missing
/// session called `5` for a request that named none. The same held for
/// `debug --window 0x123`.
#[test]
fn an_option_value_is_not_resolved_as_the_session_name() {
    runtime_dir();
    for (args, forbidden) in [
        (vec!["logs", "-n", "5"], "5"),
        (vec!["debug", "--window", "0x123"], "0x123"),
        (vec!["logs", "-n", "5", "--xserver"], "5"),
    ] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_maverickctl"))
            .args(&args)
            .output()
            .expect("run maverickctl");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_ne!(
            out.status.code(),
            Some(0),
            "`maverickctl {args:?}` must fail: no session was named"
        );
        assert!(
            !stderr.contains(&format!("session '{forbidden}'")),
            "`maverickctl {args:?}` resolved the option's value ({forbidden}) as the \
             session name: {stderr}"
        );
    }
}

/// A global option before the verb must not push the verb into the arguments.
///
/// `run_group` lifts globals to the front of `args`, and both group dispatchers
/// took `args[1..]` as "the rest". With a global in front, index 1 is still the
/// verb, so `window --json list debug` handed `session_target` a list that
/// began with `list` and it resolved the *verb* as the session name.
#[test]
fn a_global_before_the_group_verb_does_not_become_the_session_name() {
    runtime_dir();
    for (args, forbidden) in [
        (vec!["window", "--json", "list", "sessA"], "list"),
        (vec!["session", "--json", "status", "sessA"], "status"),
        (vec!["window", "-j", "list", "sessA"], "list"),
    ] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_maverickctl"))
            .args(&args)
            .output()
            .expect("run maverickctl");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains(&format!("session '{forbidden}'")),
            "`maverickctl {args:?}` resolved the verb ({forbidden}) as the session name: \
             {stderr}"
        );
    }
}

/// The session named after the verb is still the one used.
///
/// The correction above moves where the arguments after the verb start; this
/// pins that it did not move them somewhere else.
#[test]
fn the_session_after_the_verb_is_still_resolved() {
    // A fresh directory: the fixtures above leave live records behind, and a
    // command that resolves its session from *context* would then report an
    // ambiguity instead of the missing session this asserts on.
    //
    // It is handed to the child rather than published through the environment.
    // `XDG_RUNTIME_DIR` is one variable for the whole binary and the tests run
    // in parallel: setting it here would move the directory every sibling test
    // resolves against, out from under a fixture that had already been bound
    // there — which is a connection refused because the socket it published is
    // gone, not a verdict about option parsing.
    let dir = runtime_dir::dir_for("maverick-cli-after-verb");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("fresh runtime dir");
    for args in [
        vec!["window", "list", "sessA"],
        vec!["window", "--json", "list", "sessA"],
        vec!["logs", "sessA"],
        vec!["session", "status", "sessA"],
        vec!["inspect", "sessA"],
        vec!["camera", "sessA", "left"],
    ] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_maverickctl"))
            .env("XDG_RUNTIME_DIR", &dir)
            .args(&args)
            .output()
            .expect("run maverickctl");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("session 'sessA'"),
            "`maverickctl {args:?}` must name the session it was given, got: {stderr}"
        );
    }
}

/// `-n` keeps its documented meaning in `logs` and its meaning elsewhere.
///
/// The two are different options that share a spelling: `logs`'s `-n` is a line
/// count, while `--name`/`-n` elsewhere names an instance. Both must keep
/// working, and neither may be able to read the other's value.
#[test]
fn the_two_meanings_of_dash_n_stay_separate() {
    runtime_dir();
    // `logs --name <sid>` is the instance selector and must resolve that session,
    // not treat the name as a line count.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_maverickctl"))
        .args(["logs", "--name", "namedSession"])
        .output()
        .expect("run maverickctl");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("session 'namedSession'"),
        "`logs --name` must select the named session, got: {stderr}"
    );
    assert!(
        !stderr.contains("line"),
        "`--name` must not be read as a line count: {stderr}"
    );

    // `logs -n 5` is a line count; the missing session must be reported as
    // context resolution, never as a session named `5`.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_maverickctl"))
        .args(["logs", "-n", "5"])
        .output()
        .expect("run maverickctl");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("session '5'"),
        "`-n 5` is five lines, not a session called 5: {stderr}"
    );
}

/// An option with a missing value is a parse error, not a silent argument.
///
/// `--name` with nothing after it consumed the *verb* as its value in the
/// positional parser, so the command ran against the wrong target instead of
/// reporting the mistake.
#[test]
fn a_value_option_without_a_value_is_reported() {
    runtime_dir();
    for args in [vec!["msg"], vec!["msg", "--name"]] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_maverickctl"))
            .args(&args)
            .output()
            .expect("run maverickctl");
        assert_ne!(
            out.status.code(),
            Some(0),
            "`maverickctl {args:?}` must fail rather than act on a missing value"
        );
    }
}

/// The usage page must describe the option scopes it actually implements.
#[test]
fn the_usage_page_documents_the_option_scopes() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_maverickctl"))
        .arg("--help")
        .output()
        .expect("run maverickctl");
    let page = String::from_utf8_lossy(&out.stdout);
    // `--json` is a global of this tool, so the page has to say so rather than
    // leaving a reader to infer it from a per-command example.
    assert!(
        page.contains("--json"),
        "the usage page must document --json: {page}"
    );
    // The instance selector is documented under its own heading.
    assert!(
        page.contains("INSTANCE SELECTION"),
        "the usage page must document how an instance is selected"
    );
    assert!(
        page.contains("--name <id>"),
        "the usage page must document --name"
    );
}

/// A selector that matches nothing has to say which selector missed.
///
/// `--session` and `--name` address the same object by different keys, and both
/// resolve before anything is connected: a miss here is a *resolution* failure,
/// which is a different layer from a refused socket or an unanswered request.
/// Asserting only that the command failed cannot tell those three apart — every
/// one of them exits non-zero — which is how `--name` came to exit non-zero with
/// nothing on either stream while `--session` beside it named the session that
/// was missing.
///
/// So each arm asserts the whole observable: the exit status, the empty stdout
/// (a diagnostic on stdout would vanish under `maverickctl 2>/dev/null`), and
/// the exact stderr, which also pins that one failure produces one message.
#[test]
fn a_selector_that_matches_nothing_names_the_selector_that_missed() {
    let dir = runtime_dir();
    // Names nothing on any machine and cannot be produced by another arm of the
    // resolver, so a failure here can only be this lookup — not the context
    // fallback, and not a socket or protocol error.
    const MISSING: &str = "__maverick_no_such_instance__";
    let missing_sid = format!("{MISSING}_session");

    for (flag, selector, expected) in [
        (
            "--name",
            MISSING,
            format!("maverickctl: no instance named '{MISSING}'"),
        ),
        (
            "--session",
            missing_sid.as_str(),
            format!("maverickctl: no instance with session id '{missing_sid}'"),
        ),
    ] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_maverickctl"))
            .env("XDG_RUNTIME_DIR", &dir)
            .args([flag, selector, "query", "state"])
            .output()
            .expect("run maverickctl");
        assert_eq!(
            out.status.code(),
            Some(1),
            "{flag} must fail on a target that matches nothing"
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "",
            "{flag} resolves before it reports: stdout must stay empty"
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stderr).trim(),
            expected,
            "{flag} must name the selector that missed, and say it once"
        );
    }
}

/// A record whose socket is gone is still a target, and the tool still says so.
///
/// Liveness is a ping, so `list_instances` hands every caller an `alive` flag and
/// leaves the judgement to them: the context arm of `resolve_target` and
/// `quit_all` filter on it, and the session chain checks it itself and reports
/// `NotFound`. The explicit `--name`/`--session` arms do not, because across an
/// `exec` the record deliberately outlives its socket — `restart_lifecycle`
/// pins that as "a handoff leaves a resolvable target, not an absent one" — so
/// a session part-way through a restart is not a session that is gone. The
/// failure therefore belongs to the connection layer, and this asserts it landed
/// there.
///
/// Gating resolution on `alive` would be wrong for a second, sharper reason:
/// since liveness *is* a ping, a peer that is bound and accepting but does not
/// answer `ping` also reads as stale, and its target would be reported as not
/// existing at all. `a_reachable_peer_that_answers_nothing_is_a_failed_command`
/// fails on exactly that mutation.
#[test]
fn a_stale_record_is_still_a_resolvable_target() {
    let dir = runtime_dir();
    let sid = "staleremembered";
    // A record with no socket behind it: published, resolvable, not alive.
    // `alive: true` is what the window manager wrote, and discovery overwrites
    // it — which is the point, so the fixture cannot pass by being declared
    // stale rather than being measured stale.
    maverick_sys::identity::write_meta(&InstanceInfo {
        name: sid.to_string(),
        session_id: sid.to_string(),
        pid: std::process::id(),
        display: String::new(),
        tty_nr: 0,
        x_server_identity: String::new(),
        start_time: 0,
        exe: String::new(),
        started_at: 0,
        alive: true,
    })
    .expect("write a record with no socket");

    // Each precondition is checked, so a fixture that quietly became "no record
    // at all" or "still listening" cannot satisfy this.
    assert!(
        maverick_sys::identity::read_meta(sid).is_some(),
        "the record must exist: a missing target is a different case"
    );
    assert!(
        !maverick_sys::identity::sock_path(sid).exists(),
        "the socket must be gone: a listening peer is a different case"
    );
    let found = maverickctl::discover::find_by_name(sid).expect("a record is still a target");
    assert!(
        !found.alive,
        "the fixture must measure as stale, or this asserts nothing"
    );

    for flag in ["--name", "--session"] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_maverickctl"))
            .env("XDG_RUNTIME_DIR", &dir)
            .args([flag, sid, "query", "state"])
            .output()
            .expect("run maverickctl");
        assert_eq!(
            out.status.code(),
            Some(1),
            "{flag} must fail against a target that is not answering"
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "",
            "{flag} resolves before it reports: stdout must stay empty"
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stderr).trim(),
            "maverickctl: query failed: No such file or directory (os error 2)",
            "{flag} resolved the record, so the failure is the connection, not the target"
        );
    }

    // This fixture writes a record directly rather than through a handle, so it
    // withdraws it directly too: `cleanup_meta` is scoped to this session and
    // unlinks the record and nothing else, and leaving it published would make
    // this the one test in the binary that accumulates a file per run — and an
    // instance nothing owns, which is the thing the ownership work has been
    // removing. There is no socket to unlink; the record is all this published.
    maverick_sys::identity::cleanup_meta(sid);
}
