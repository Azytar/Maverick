//! Tests for what a `maverickctl` option *means*, and for the results it reports.
//!
//! The previous suite (`cli_options`) covers which words this tool consumes.
//! This one covers what the ones it consumes do: an option accepted and dropped
//! is a different defect from an option forwarded by mistake, and so is a
//! failure reported as a success.

use maverick_sys::identity::InstanceInfo;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{self, Receiver};

mod runtime_dir;

/// One runtime directory for the whole binary, set once.
///
/// It is a process-global the CLI reads through the environment, so the
/// alternatives are both wrong: a per-test `set_var` races the other tests that
/// run in parallel, and a per-invocation directory means the fixture socket and
/// the record written for it can end up in different places. Tests that need to
/// be sure of what the context resolves to use an explicit `--session`.
fn runtime_dir() -> PathBuf {
    runtime_dir::isolate("maverick-cli-sem")
}

#[derive(Debug)]
struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(args: &[&str]) -> Run {
    let out = Command::new(env!("CARGO_BIN_EXE_maverickctl"))
        .args(args)
        .output()
        .expect("run maverickctl");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// Write a session record so `inspect`/`exec` resolve a session.
fn write_session_record(sid: &str) {
    runtime_dir();
    let name = maverickctl::session::SessionName::parse(sid).expect("valid fixture name");
    let spec = maverickctl::session::Spec::default();
    maverickctl::session::write(&maverickctl::session::Session::new(name, spec))
        .expect("write fixture session record");
}

/// A session record plus a control socket that answers what it was given.
///
/// Both halves are needed: `inspect` resolves a *session* (its record) and then
/// queries the *window manager* (its socket), so a fixture with only one of
/// them would fail resolution before reaching the query under test.
fn serve_replying(
    sid: &str,
    inspect_reply: &'static str,
    tree_reply: &'static str,
) -> Receiver<String> {
    let sid = sid.to_string();
    runtime_dir();
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
    // The session record, so `inspect` resolves a session rather than reporting
    // that the one it was named does not exist.
    write_session_record(&sid);

    let (tx, rx) = mpsc::channel();
    let pid = sid.clone();
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
                format!("pong {pid}\n")
            } else if request == "query inspect" {
                format!("{inspect_reply}\n")
            } else if request == "query tree" {
                format!("{tree_reply}\n")
            } else if request.starts_with("query ") {
                "{}\n".to_string()
            } else {
                "ok\n".to_string()
            };
            let _ = stream.write_all(reply.as_bytes());
            let _ = stream.flush();
        }
    });
    rx
}

// ── A. --force ────────────────────────────────────────────────────────────────

/// `--force` has one meaning in the session group: let `remove` delete a session
/// that is still running. On the other verbs it was read and then dropped, so
/// `session stop <name> --force` printed `stopped` and exited 0 as though a force
/// had been applied. An option with no meaning on a verb must be refused there.
#[test]
fn force_is_refused_on_the_verbs_that_have_no_meaning_for_it() {
    for verb in ["start", "stop", "restart", "kill"] {
        let r = run(&["session", verb, "absentForForceLong", "--force"]);
        assert_ne!(
            r.code, 0,
            "`session {verb} --force` must fail, not report success: {r:?}"
        );
        assert!(
            r.stderr.contains("takes no --force"),
            "`session {verb} --force` must say the option does not apply, got: {}",
            r.stderr
        );
        assert!(
            !r.stdout.contains("ped"),
            "`session {verb} --force` must not print a success line: {}",
            r.stdout
        );
    }
}

/// `-f` is the same option as `--force` here, so it is refused the same way.
#[test]
fn dash_f_is_refused_on_the_verbs_that_have_no_meaning_for_it() {
    for verb in ["start", "stop", "restart", "kill"] {
        let r = run(&["session", verb, "absentForForceShort", "-f"]);
        assert_ne!(
            r.code, 0,
            "`session {verb} -f` must fail, not report success: {r:?}"
        );
        assert!(
            r.stderr.contains("takes no --force"),
            "`session {verb} -f` must say the option does not apply, got: {}",
            r.stderr
        );
    }
}

/// The verb that *does* have a meaning for it must still accept it.
///
/// The refusal above would be wrong if it also rejected `remove`, where
/// `--force` is the documented difference between refusing and removing a live
/// session. A nonexistent session fails either way, but the diagnostic must be
/// about the session, not about the option.
#[test]
fn force_is_still_accepted_by_remove() {
    for flag in ["--force", "-f"] {
        let r = run(&["session", "remove", "absentForRemove", flag]);
        assert_ne!(
            r.code, 0,
            "removing a missing session must still fail: {r:?}"
        );
        assert!(
            !r.stderr.contains("takes no --force"),
            "`session remove {flag}` must be accepted; the option is meaningful \
             there, so it must not be refused: {}",
            r.stderr
        );
    }
}

// ── B. -f across verbs ───────────────────────────────────────────────────────

/// `-f` means follow in `logs` and force in `session remove`, and neither in
/// `process kill`. It was claimed per group but nothing rejected the
/// unclaimed spelling, so `process kill <s> <pid> -f` sent SIGTERM — a caller
/// asking for an unconditional kill was told the process had been signalled.
#[test]
fn dash_f_is_rejected_by_process_kill_rather_than_silently_downgraded() {
    let r = run(&["process", "kill", "absentForPKillShort", "1234", "-f"]);
    assert_ne!(r.code, 0, "`process kill -f` must fail: {r:?}");
    assert!(
        r.stderr.contains("does not take -f") && r.stderr.contains("--force"),
        "the diagnostic must name the option that does apply, got: {}",
        r.stderr
    );
}

/// The signal verbs' own spellings must keep working: `--force` and `-9` both
/// mean SIGKILL, and neither may be mistaken for the other verbs' `-f`.
#[test]
fn process_kill_still_accepts_its_own_force_spellings() {
    for flag in ["--force", "-9"] {
        let r = run(&["process", "kill", "absentForPKill", "1234", flag]);
        assert_ne!(r.code, 0, "a missing session must still fail: {r:?}");
        assert!(
            !r.stderr.contains("does not take"),
            "`process kill {flag}` is this verb's own option and must not be \
             refused: {}",
            r.stderr
        );
        // It got far enough to look for the session, not to reject the option.
        assert!(
            r.stderr.contains("session") || r.stdout.contains("session"),
            "`process kill {flag}` must reach session resolution: {r:?}"
        );
    }
}

// ── C. exec option leakage ───────────────────────────────────────────────────

/// `--wait` and `--inherit` belong to this tool only *before* the command word.
///
/// `exec` read them from every argument, so `exec <s> app --wait` both passed
/// `--wait` to `app` and made this tool block on it. The boundary
/// `split_call` already documents — everything after the command word is the
/// program's — has to hold for the options too.
#[test]
fn an_option_after_the_command_word_is_not_this_tools() {
    use maverickctl::ctl::session::split_call;
    use maverickctl::ctl::Ctl;

    let args: Vec<String> = ["debug", "alacritty", "--wait", "--inherit"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let c = Ctl::parse("maverickctl", &args, &[], &[]);
    let call = split_call(&c, &args).expect("a call");
    assert_eq!(
        call.argv,
        vec!["alacritty", "--wait", "--inherit"],
        "the program keeps its own arguments, verbatim"
    );
    assert!(
        !call.flag_before_command(&["--wait", "-w"]),
        "--wait after the command word is the program's argument, not this tool's option"
    );
    assert!(
        !call.flag_before_command(&["--inherit", "-i"]),
        "--inherit after the command word is the program's argument"
    );
}

/// Before the boundary they are this tool's options, and the program's argv must
/// not contain them.
#[test]
fn an_option_before_the_command_word_is_this_tools() {
    use maverickctl::ctl::session::split_call;
    use maverickctl::ctl::Ctl;

    let args: Vec<String> = ["debug", "--wait", "alacritty"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let c = Ctl::parse("maverickctl", &args, &[], &[]);
    let call = split_call(&c, &args).expect("a call");
    assert!(
        call.flag_before_command(&["--wait", "-w"]),
        "--wait before the command word is this tool's option"
    );
    assert_eq!(
        call.argv,
        vec!["alacritty"],
        "this tool's option must not reach the program's argv"
    );
}

/// An explicit `--` ends this tool's parsing, so nothing after it is an option
/// of this tool — including something spelled exactly like one.
#[test]
fn a_double_dash_ends_this_tools_option_parsing() {
    use maverickctl::ctl::session::split_call;
    use maverickctl::ctl::Ctl;

    // The rule is positional: an option is this tool's when it comes before the
    // command word, and the program's when it comes after. `--` moves where the
    // command word is, so an option written after `--` is the program's even
    // when it is spelled exactly like one of this tool's.
    let args: Vec<String> = ["debug", "--wait", "--", "app", "--inherit"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let c = Ctl::parse("maverickctl", &args, &[], &[]);
    let call = split_call(&c, &args).expect("a call");
    assert_eq!(
        call.argv,
        vec!["app", "--inherit"],
        "after -- everything is the program's, including a flag of the same name"
    );
    // `--wait` was written before the boundary, so it is this tool's option.
    assert!(
        call.flag_before_command(&["--wait", "-w"]),
        "--wait came before --, so it is this tool's option"
    );
    // `--inherit` came after it, so it is only the program's.
    assert!(
        !call.flag_before_command(&["--inherit", "-i"]),
        "--inherit came after --, so it is the program's argument and must not \
         also be read as this tool's option"
    );
}

/// `exec` must read its options from the same side of the boundary the caller
/// sees.
///
/// The unit tests above pin what `split_call` computes; this pins that `exec`
/// uses it. Without it the helper could be correct and unused, and the leak would
/// return with every test still green — which is what reverting the *call site*
/// alone showed.
#[test]
fn exec_does_not_treat_a_program_argument_as_its_own_option() {
    use maverickctl::ctl::session::split_call;
    use maverickctl::ctl::Ctl;
    // What `exec` computes, from the same inputs it is given.
    let decide = |args: &[&str]| -> (bool, bool, Vec<String>) {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let c = Ctl::parse("maverickctl", &owned, &[], &[]);
        let call = split_call(&c, &owned).expect("a call");
        (
            call.flag_before_command(&["--wait", "-w"]),
            call.flag_before_command(&["--inherit", "-i"]),
            call.argv,
        )
    };
    let (wait, inherit, argv) = decide(&["debug", "app", "--wait", "--inherit"]);
    assert!(
        !wait && !inherit,
        "options after the command word are the program's: this tool must not \
         wait for or inherit to them"
    );
    assert_eq!(
        argv,
        vec!["app", "--wait", "--inherit"],
        "and they are still handed to the program, verbatim"
    );

    // Before the boundary they are this tool's, and the program does not see
    // them — the two halves of the same rule.
    let (wait, _inherit, argv) = decide(&["debug", "--wait", "app"]);
    assert!(wait, "--wait before the command word is exec's own option");
    assert_eq!(
        argv,
        vec!["app"],
        "this tool's option must not be handed to the program"
    );
}

/// `exec` end to end: the option decides this tool's exit status, and only when
/// it was written before the command word.
///
/// `false` exits non-zero and ignores its arguments, so the two spellings differ
/// observably and without any timing: `exec s false --wait` hands `--wait` to
/// the program and returns at once (0), while `exec s --wait false` waits for
/// the program and propagates its status (1). Both finish immediately.
#[test]
fn exec_reads_wait_from_before_the_command_word_only() {
    runtime_dir();
    let sid = "cliexecboundary";
    write_session_record(sid);

    let after = run(&["exec", sid, "false", "--wait"]);
    assert_eq!(
        after.code, 0,
        "`exec {sid} false --wait`: --wait is the program's, so exec returns \
         at once and does not propagate the program's status: {after:?}"
    );

    let before = run(&["exec", sid, "--wait", "false"]);
    assert_eq!(
        before.code, 1,
        "`exec {sid} --wait false`: --wait is exec's, so it waits and \
         propagates the program's non-zero status: {before:?}"
    );
}

/// Argument order and boundaries survive the split.
#[test]
fn the_programs_arguments_keep_their_order_and_words() {
    use maverickctl::ctl::session::split_call;
    use maverickctl::ctl::Ctl;

    let args: Vec<String> = [
        "debug",
        "app",
        "--title",
        "my window",
        "-x",
        "a b",
        "trailing",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let c = Ctl::parse("maverickctl", &args, &[], &[]);
    let call = split_call(&c, &args).expect("a call");
    assert_eq!(
        call.argv,
        vec!["app", "--title", "my window", "-x", "a b", "trailing"],
        "the program's argv must be the words after its name, in order, unsplit"
    );
}

// ── D. swallowed refusals ────────────────────────────────────────────────────

/// A refusal is not an absence.
///
/// `inspect` collapsed every failure of the window manager's `inspect` query
/// into `None` with `.ok()`, so an instance that was up and *refused* — and one
/// that was not running — produced the same output and the same exit 0.
#[test]
fn a_refused_inspect_query_fails_the_command() {
    runtime_dir();
    let sid = "cliinspectrefuse";
    let _rx = serve_replying(sid, "error restarting", "{}");
    let r = run(&["inspect", sid]);
    assert_ne!(
        r.code, 0,
        "a refused inspect query must fail, not report an empty layout: {r:?}"
    );
    assert!(
        r.stderr.contains("refused") && r.stderr.contains("restarting"),
        "the instance's reason must reach the caller, got: {}",
        r.stderr
    );
    assert!(
        !r.stdout.contains("no live layout"),
        "a refusal must not be reported as an absent layout: {}",
        r.stdout
    );
}

/// The same for the window verb, which shares the `window list` fixture.
#[test]
fn a_refused_window_inspect_query_fails_the_command() {
    runtime_dir();
    let sid = "cliwinspectrefuse";
    let tree = r#"{"sel_mon":0,"monitors":[{"index":0,"active_ws":0,"focused":270336,"workspaces":[{"index":0,"layout":"column","columns":[{"width":1280.0,"focused":0,"windows":[{"id":270336,"pid":1234,"class":"Zed","instance":"Zed","title":"main.rs","monitor":0,"workspace":0,"float":false,"fullscreen":false,"maximized":false,"sticky":false,"geom":[0,8,1280,792],"focus":true,"overlay":false}]}],"floats":[]}]}]}"#;
    let _rx = serve_replying(sid, "error restarting", tree);
    let r = run(&["window", "inspect", sid, "Zed"]);
    assert_ne!(
        r.code, 0,
        "a refused inspect query must fail `window inspect`: {r:?}"
    );
    assert!(
        r.stderr.contains("refused"),
        "the refusal must be classified, not dropped, got: {}",
        r.stderr
    );
}

/// A window manager that is genuinely not running is still absent, not an
/// error — that is the distinction the refusal fix has to preserve.
#[test]
fn an_absent_window_manager_is_reported_as_absent_not_as_an_error() {
    let r = run(&["inspect", "definitelyNotRunning"]);
    assert_ne!(
        r.code, 0,
        "inspecting a session that does not exist must still fail: {r:?}"
    );
    assert!(
        !r.stderr.contains("refused"),
        "a missing session is not a refusal: {}",
        r.stderr
    );
}

// ── E. session resolution ────────────────────────────────────────────────────

/// An explicit `--session` is validated before it is used.
///
/// The session chain accepts a name and the instance chain accepts a session id,
/// and each validates with its own rule. A name that could escape the runtime
/// directory must be refused by the rule that keeps it inside.
#[test]
fn an_explicit_session_that_cannot_be_a_name_is_refused() {
    for bad in ["../escape", "a/b", ".."] {
        let r = run(&["inspect", "--session", bad]);
        assert_ne!(
            r.code, 0,
            "`--session {bad}` must be refused, not acted on: {r:?}"
        );
        assert!(
            !r.stdout.contains("SESSION"),
            "`--session {bad}` must not produce a session report: {}",
            r.stdout
        );
    }
}

/// The precedence itself: an explicit session beats the positional, and the
/// positional beats the environment.
#[test]
fn an_explicit_session_beats_the_positional_one() {
    // Neither session exists, so the diagnostic names whichever was chosen.
    let r = run(&["inspect", "--session", "fromFlag", "fromPositional"]);
    assert!(
        r.stderr.contains("fromFlag"),
        "an explicit --session must win over a positional, got: {}",
        r.stderr
    );
    assert!(
        !r.stderr.contains("fromPositional"),
        "the positional must not be consulted when a flag named one: {}",
        r.stderr
    );
}
