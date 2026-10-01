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

use maverickctl::ctl::main_with_args;
use std::path::PathBuf;
use std::process::ExitCode;

mod instance;
mod runtime_dir;

use instance::{Instance, Published};

/// This binary's private runtime directory, so no fixture here is visible to
/// another test binary's and no live instance can be discovered.
fn runtime_dir() -> PathBuf {
    runtime_dir::isolate("maverick-ctl-refusals")
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
///
/// Unbounded, and for as long as the test holds the handle: a fixture that ran
/// out of replies would fail as "connection refused", passing a verb that is not
/// really being exercised for the wrong reason.
fn serve(sid: &str, tree_reply: &'static str, dispatch_reply: &'static str) -> Instance {
    // The worker outlives this call, so it owns its copy of everything it
    // answers with rather than borrowing the caller's strings.
    let owned_sid = sid.to_string();
    let tree_reply = tree_reply.to_string();
    let dispatch_reply = dispatch_reply.to_string();
    Instance::serve(sid, Published::Instance, None, move |request| {
        Some(if request == "ping" {
            format!("pong {owned_sid}\n")
        } else if request == "query tree" {
            format!("{tree_reply}\n")
        } else if request.starts_with("query ") {
            "{}\n".to_string()
        } else if request.starts_with("dispatch") {
            format!("{dispatch_reply}\n")
        } else {
            "error unknown-command: fixture\n".to_string()
        })
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

/// A dropped fixture takes its socket, its record and its worker with it.
///
/// The handle is what makes this a fixture rather than a side effect. Without
/// it the listener keeps answering and the record keeps reading `alive` for the
/// rest of the binary, so the next test that stands up an instance is doing so
/// beside one it did not ask for — and nothing reports it, because a leftover
/// instance is only visible to whatever resolves it afterwards.
#[test]
fn a_dropped_refusal_fixture_leaves_nothing_discoverable() {
    runtime_dir();
    let sid = "refusaldrop";
    let sock = maverick_sys::identity::sock_path(sid);
    assert!(
        maverickctl::discover::find_by_name(sid).is_none(),
        "the fixture's namespace must start empty, or this proves nothing"
    );
    {
        let server = serve(sid, TREE, "error busy: command queue full");
        assert!(sock.exists(), "the fixture must publish a socket");
        assert!(
            maverickctl::client::ping(sid).is_ok(),
            "the fixture must answer while it is held"
        );
        assert!(
            maverickctl::discover::find_by_name(sid).is_some(),
            "the fixture must be discoverable while it is held"
        );
        drop(server);
    }
    assert!(!sock.exists(), "the socket outlived its owner: {sock:?}");
    assert!(
        maverickctl::discover::find_by_name(sid).is_none(),
        "the instance is still discoverable after its owner was dropped"
    );
    assert!(
        maverickctl::client::ping(sid).is_err(),
        "something is still answering on a socket its owner withdrew"
    );
}

/// A teardown must not reach past its own session.
///
/// Every fixture in this binary shares one runtime directory, so a cleanup that
/// named a different session would delete a neighbour's socket and its record —
/// which is how a test starts failing because another one tidied up.
#[test]
fn a_dropped_refusal_fixture_spares_the_others() {
    runtime_dir();
    let (mine, theirs) = ("refusalspare-mine", "refusalspare-theirs");
    let other = serve(theirs, TREE, "ok");
    let sock = maverick_sys::identity::sock_path(theirs);
    {
        let _own = serve(mine, TREE, "ok");
    }
    assert!(
        sock.exists(),
        "dropping one fixture removed a session it never created: {sock:?}"
    );
    assert!(
        maverickctl::discover::find_by_name(theirs).is_some(),
        "dropping one fixture removed another session's record"
    );
    drop(other);
}
