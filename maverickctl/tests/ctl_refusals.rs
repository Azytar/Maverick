//! What `maverickctl` does with a refusal the instance has already put on the
//! wire.
//!
//! A control-protocol refusal is not a transport failure: the socket exchange
//! completes and the *body* of the reply is `error …`. A verb that classifies
//! only the socket therefore cannot tell a refusal from a success, and reports
//! one as the operation it asked for — which for the window verbs is a printed
//! window id, a `--json` document naming the action, and exit 0.
//!
//! So these tests stand up a fixture that answers over the real line protocol,
//! with the real refusal strings the server produces, and assert that each verb
//! reaches the shell's exit status. Separate from `ctl_replies` because it is a
//! different claim: that one says an `error` body is not data, this one says no
//! verb may swallow one.

use maverick_sys::identity::InstanceInfo;
use maverickctl::ctl::main_with_args;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::ExitCode;

/// A private runtime directory, so nothing here is visible to another test
/// binary's fixtures and no live instance can be discovered.
fn runtime_dir() -> PathBuf {
    use std::sync::{Mutex, OnceLock};
    static DIR: OnceLock<Mutex<PathBuf>> = OnceLock::new();
    let cell = DIR.get_or_init(|| {
        let dir =
            std::env::temp_dir().join(format!("maverick-ctl-refusals-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("XDG_RUNTIME_DIR", &dir);
        Mutex::new(dir)
    });
    cell.lock().expect("runtime dir lock").clone()
}

/// A `tree` document, captured from a running session rather than written to
/// suit the reader: the flattening tests in the crate guard the same shape.
const TREE: &str = r#"{"sel_mon":0,"monitors":[{"index":0,"active_ws":0,"focused":270336,"workspaces":[{"index":0,"layout":"column","columns":[{"width":1280.0,"focused":0,"windows":[{"id":270336,"pid":1234,"class":"Zed","instance":"Zed","title":"main.rs","monitor":0,"workspace":0,"float":false,"fullscreen":false,"maximized":false,"sticky":false,"geom":[0,8,1280,792],"focus":true,"overlay":false}]}],"floats":[]}]}]}"#;

/// Stand in for an instance, answering every verb over the real protocol.
///
/// The two refusals are the ones `maverick_sys`'s `dispatch_line` actually
/// writes: a full window-manager command queue for the queued action, and
/// `error restarting` for a read once the instance has begun its handoff. A test
/// chooses between them by asking for the receipt `ok` instead, so the two
/// shapes the wire has — receipt and refusal — are both reachable.
fn serve(
    sid: &str,
    tree_reply: &'static str,
    dispatch_reply: &'static str,
) -> std::thread::JoinHandle<()> {
    let sid = sid.to_string();
    let path = maverick_sys::identity::sock_path(&sid);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create fixture session dir");
    }
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).expect("bind fixture socket");
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
    .expect("write fixture ficha");
    std::thread::spawn(move || {
        // Unbounded, and for the life of the test binary: several tests share
        // this thread-per-connection loop pattern, and a fixture that ran out of
        // replies would fail as "connection refused" — passing a verb that is
        // not really being exercised for the wrong reason.
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let reply = {
                let mut reader = BufReader::new(&stream);
                let mut request = String::new();
                if reader.read_line(&mut request).is_err() {
                    continue;
                }
                let request = request.trim().to_string();
                if request == "ping" {
                    format!("pong {sid}\n")
                } else if request == "query tree" {
                    format!("{tree_reply}\n")
                } else if request.starts_with("query ") {
                    "{}\n".to_string()
                } else if request.starts_with("dispatch") {
                    format!("{dispatch_reply}\n")
                } else {
                    "error unknown-command: fixture\n".to_string()
                }
            };
            let _ = stream.write_all(reply.as_bytes());
            let _ = stream.flush();
        }
    })
}

/// Every window and layout verb, with the arguments that make each one act.
///
/// All of them end in the same place — a `dispatch` the instance may refuse —
/// and each is a separate command a caller runs on its own, so each is a
/// separate chance to lose the refusal.
fn verbs(sid: &str) -> Vec<(&'static str, Vec<String>)> {
    let words = |ws: &[&str]| ws.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    vec![
        ("window focus", words(&["window", "focus", sid, "Zed"])),
        ("window close", words(&["window", "close", sid, "Zed"])),
        ("window float", words(&["window", "float", sid, "Zed"])),
        (
            "window fullscreen",
            words(&["window", "fullscreen", sid, "Zed"]),
        ),
        (
            "window move",
            words(&["window", "move", sid, "Zed", "right"]),
        ),
        ("camera", words(&["camera", sid, "right"])),
        ("resize", words(&["resize", sid, "+10%"])),
        ("layout", words(&["layout", sid, "column"])),
    ]
}

/// A refusal the instance put on the wire is a failed command.
///
/// Each verb printed the result it had asked for — `0x…: focus`, `camera right`,
/// `layout column` — and exited 0, so a script driving a window saw the operation
/// it intended rather than the one the instance declined.
#[test]
fn a_refused_window_action_is_a_failed_command() {
    runtime_dir();
    let sid = "refusedwindow";
    let _server = serve(sid, TREE, "error busy: command queue full");
    for (label, argv) in verbs(sid) {
        assert_eq!(
            main_with_args("maverickctl", argv.clone()),
            ExitCode::FAILURE,
            "`maverickctl {argv:?}` reported a refused {label} as success"
        );
    }
}

/// The other half of the same claim: an accepted action is still a success.
///
/// Without this, "classify every reply" and "always fail" are indistinguishable,
/// and the fix would cost every window command its whole purpose.
#[test]
fn an_accepted_window_action_is_a_successful_command() {
    runtime_dir();
    let sid = "acceptedwindow";
    let _server = serve(sid, TREE, "ok");
    for (label, argv) in verbs(sid) {
        assert_eq!(
            main_with_args("maverickctl", argv.clone()),
            ExitCode::SUCCESS,
            "`maverickctl {argv:?}` failed an accepted {label}"
        );
    }
}

/// The instance's own words reach the caller, not a generic failure.
///
/// The exit status says the command did not happen; the message says why, and it
/// is the reason the window manager gave — a queue it could not accept, a
/// session mid-restart. A generic message would leave the caller with no way to
/// tell "try again" from "that window is gone".
#[test]
fn the_reason_the_instance_gave_reaches_the_caller() {
    let dir = runtime_dir();
    let sid = "reasonwindow";
    let _server = serve(sid, TREE, "error busy: command queue full");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_maverickctl"))
        .env("XDG_RUNTIME_DIR", &dir)
        .args(["window", "focus", sid, "Zed"])
        .output()
        .expect("run maverickctl");
    assert!(!out.status.success(), "a refused focus must not exit 0");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("command queue full"),
        "the instance's own reason must reach the caller: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("focus"),
        "a refused action must not be reported as performed: {stdout}"
    );
}

/// The same rule for the tree read every window verb makes first.
///
/// A refusal there used to reach the JSON parser, so an instance that said
/// `error restarting` was reported as having sent a window tree that was "not
/// valid JSON" — a diagnosis of the instance's output where the instance had in
/// fact given a reason.
#[test]
fn a_refused_tree_read_names_the_reason() {
    let dir = runtime_dir();
    let sid = "refusedtree";
    // An instance that has begun its restart hands every read back as
    // `error restarting`, and the dispatch it would follow is still accepted —
    // so the tree read is the only thing that can fail here.
    let _server = serve(sid, "error restarting", "ok");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_maverickctl"))
        .env("XDG_RUNTIME_DIR", &dir)
        .args(["window", "list", sid])
        .output()
        .expect("run maverickctl");
    assert!(!out.status.success(), "a refused tree read must not exit 0");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("restarting"),
        "the instance\'s own reason must be reported: {stderr}"
    );
    assert!(
        !stderr.contains("not valid JSON"),
        "a refusal is not a malformed document: {stderr}"
    );
}

/// A `dispatch` the instance refused fails; the receipt for one it accepted does
/// not.
///
/// This is the whole exit-status contract of the two dispatch paths — `msg` and
/// the verbatim-forwarding one. `ok` is a *queue receipt*: the instance took the
/// request, and the action grammar is applied afterwards on the window manager's
/// own thread, which this process cannot observe. So the tool reports refusal and
/// nothing more, and pins that boundary here rather than implying it decided
/// whether the action meant anything.
#[test]
fn a_refused_dispatch_fails_and_a_receipt_does_not() {
    runtime_dir();
    let refused = serve("refusedmsg", TREE, "error busy: command queue full");
    let accepted = serve("acceptedmsg", TREE, "ok");
    let argv = |sid: &str, verb: &str| {
        let mut a = vec![verb.to_string(), "focus-left".to_string()];
        a.push("--name".to_string());
        a.push(sid.to_string());
        a
    };
    for verb in ["msg", "command"] {
        assert_eq!(
            main_with_args("maverickctl", argv("refusedmsg", verb)),
            ExitCode::FAILURE,
            "`{verb}` reported a refused dispatch as success"
        );
        assert_eq!(
            main_with_args("maverickctl", argv("acceptedmsg", verb)),
            ExitCode::SUCCESS,
            "`{verb}` failed a dispatch the instance accepted"
        );
    }
    // The forwarded path sends the same request the same way, so it owes the
    // caller the same answer.
    for (sid, want) in [
        ("refusedmsg", ExitCode::FAILURE),
        ("acceptedmsg", ExitCode::SUCCESS),
    ] {
        assert_eq!(
            main_with_args(
                "maverickctl",
                vec!["focus-left".into(), "--name".into(), sid.into()]
            ),
            want,
            "a forwarded action against {sid} reported the wrong status"
        );
    }
    drop(refused);
    drop(accepted);
}
