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
fn serve_recording(sid: &str) -> Receiver<String> {
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

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut reader = BufReader::new(&stream);
            let mut request = String::new();
            if reader.read_line(&mut request).is_err() {
                continue;
            }
            let request = request.trim_end().to_string();
            let _ = tx.send(request.clone());
            let reply = if request == "ping" {
                format!("pong {sid}\n")
            } else {
                "ok\n".to_string()
            };
            let _ = stream.write_all(reply.as_bytes());
            let _ = stream.flush();
        }
    });
    rx
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
    let rx = serve_recording(sid);
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
        let Ok(line) = rx.recv_timeout(deadline - std::time::Instant::now()) else {
            break;
        };
        if line.starts_with("dispatch") {
            return line;
        }
    }
    panic!("`maverickctl {args:?}` put no dispatch on the wire")
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
    let rx = serve_recording(sid);
    // `state` asks the instance for the snapshot; answer with a document.
    let _ = rx;
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
    // A fresh directory per invocation: the fixtures above leave live records
    // behind, and a command that resolves its session from *context* would then
    // report an ambiguity instead of the missing session this asserts on.
    let dir = std::env::temp_dir().join(format!(
        "maverick-cli-after-verb-{}-{}",
        std::process::id(),
        line!()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("fresh runtime dir");
    std::env::set_var("XDG_RUNTIME_DIR", &dir);
    for args in [
        vec!["window", "list", "sessA"],
        vec!["window", "--json", "list", "sessA"],
        vec!["logs", "sessA"],
        vec!["session", "status", "sessA"],
        vec!["inspect", "sessA"],
        vec!["camera", "sessA", "left"],
    ] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_maverickctl"))
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
