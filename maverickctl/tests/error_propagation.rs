//! Tests for the failures `maverickctl` must not report as successes.
//!
//! The theme is not "every `Result` is inspected" — most of them are not, and
//! that is right — but the one thing the tool promised: an operation it could
//! not complete must not exit 0. A discarded `Result` is only acceptable when
//! discarding it cannot change what the caller is told.

use maverickctl::session::SessionName;
use std::process::Command;

fn runtime_dir() -> std::path::PathBuf {
    use std::sync::{Mutex, OnceLock};
    static DIR: OnceLock<Mutex<std::path::PathBuf>> = OnceLock::new();
    let cell = DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!(
            "maverick-cli-errprop-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("XDG_RUNTIME_DIR", &dir);
        Mutex::new(dir)
    });
    cell.lock().expect("runtime dir lock").clone()
}

/// Write a session record so a session-scoped verb resolves it.
fn write_session_record(sid: &str) {
    runtime_dir();
    let name = SessionName::parse(sid).expect("valid fixture name");
    let spec = maverickctl::session::Spec::default();
    maverickctl::session::write(&maverickctl::session::Session::new(name, spec))
        .expect("write fixture session record");
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

// ── Phase 2: exec bookkeeping ───────────────────────────────────────────────

/// `stop` and `kill` must actually consult the teardown outcome.
///
/// The unit tests beside `survived_not_stopped` pin the check itself; these pin
/// that the two verbs call it. Reverting the *callers* while leaving the check
/// in place is a defect those unit tests cannot see, and it is the shape the bug
/// actually had: the helper existed and was correct, and neither verb used it.
#[test]
fn the_lifecycle_verbs_consult_the_teardown_outcome() {
    let src = include_str!("../src/session/lifecycle.rs");
    // A propagating statement, not a mention: the name applied and then `?`.
    // A definition, a doc comment, or a `let _ =` discard would all leave the
    // verb reporting success either way.
    let calls = src
        .lines()
        .filter(|l| {
            let t = l.trim();
            t.starts_with("survived_not_stopped(") && t.contains("?;")
        })
        .count();
    assert_eq!(
        calls, 2,
        "`stop` and `kill` must each propagate a surviving component, but \
         lifecycle.rs propagates it {calls} time(s)"
    );
}

/// Spawning the child and tracking the child are two results, and only the
/// second one was checked.
///
/// `proc::read` failing after a successful spawn used to skip registration
/// silently and print the pid anyway. The child *is* running, so the command
/// cannot claim the whole operation failed — but the session cannot see it, and
/// neither `process list` nor `session stop` will ever reach it, so the caller
/// must be told which of the two things went wrong.
///
/// The decision is exercised directly rather than through `/proc`: making a
/// real entry unreadable is not something a test can arrange without racing the
/// child it just spawned.
#[test]
fn a_child_that_started_but_could_not_be_read_is_reported_as_untracked() {
    use maverickctl::ctl::session::pgid_of_started_child;

    // The normal case: the child is readable, so its group is returned.
    let info =
        maverickctl::session::proc::read(std::process::id()).expect("this process is readable");
    let (pid, pgid) = (info.pid, info.pgid);
    assert_eq!(
        pgid_of_started_child(pid, "s", Some(info)).expect("a readable child is tracked"),
        pgid,
        "a child that can be read is registered with its own group"
    );

    // The failure: spawned, unreadable. This must be an error, and the message
    // must say the process is *running* — otherwise a caller reads the failure
    // as "the program did not start" and leaves a live process behind.
    let err = pgid_of_started_child(4242, "sess", None)
        .expect_err("a child that cannot be read is not tracked");
    assert!(
        err.contains("started 4242"),
        "the message must establish that the program did start: {err}"
    );
    assert!(
        err.contains("it is running"),
        "the message must say the process is alive, so it is not mistaken for a \
         failure to launch: {err}"
    );
    assert!(
        err.contains("sess"),
        "the message must name the session that cannot see it: {err}"
    );
    assert!(
        err.contains("kill 4242 yourself"),
        "the message must say how to clean the process up: {err}"
    );
}

/// The two results must be distinguishable: a program that cannot be started is
/// a different failure from one that started and could not be tracked.
#[test]
fn a_program_that_cannot_start_fails_without_claiming_a_pid() {
    runtime_dir();
    let sid = "clinostart";
    write_session_record(sid);
    let r = run(&["exec", sid, "/definitely/not/a/program"]);
    assert_ne!(r.code, 0, "a program that cannot start must fail: {r:?}");
    assert!(
        r.stdout.trim().is_empty(),
        "no pid may be printed for a program that never started: {}",
        r.stdout
    );
}

// ── Phase 1: lifecycle teardown ─────────────────────────────────────────────

/// Stopping a session that is not running is a success, and must stay one.
///
/// `stop` is the operation a script runs to be sure, so "already stopped" is
/// the state it was asking for. The teardown failure check must not turn that
/// into an error — only a component that actually survived may.
#[test]
fn stopping_a_session_that_is_already_stopped_is_a_success() {
    runtime_dir();
    let sid = "clialreadystopped";
    write_session_record(sid);
    let r = run(&["session", "stop", sid]);
    assert_eq!(
        r.code, 0,
        "stopping an already-stopped session is the state it was asked for: {r:?}"
    );
    assert!(r.stdout.contains("stopped"), "and it says so: {}", r.stdout);
}

/// Every lifecycle verb must report a failure rather than success when it
/// cannot do what it was asked.
#[test]
fn a_lifecycle_command_on_a_missing_session_fails() {
    runtime_dir();
    for verb in ["stop", "kill", "restart"] {
        let r = run(&["session", verb, "definitelyNotASession"]);
        assert_ne!(
            r.code, 0,
            "`session {verb}` on a missing session must fail: {r:?}"
        );
        assert!(
            !r.stdout.contains("ped"),
            "`session {verb}` must not print a success line: {}",
            r.stdout
        );
    }
}

/// Optional cleanup stays non-fatal.
///
/// `session remove` on a missing session fails, but that is the session being
/// absent — not a cleanup step failing. The distinction matters: a cleanup
/// failure that could not happen must not be mistaken for one that did, or the
/// teardown check would reject every remove.
#[test]
fn removing_an_absent_session_reports_the_session_not_a_cleanup_failure() {
    runtime_dir();
    let r = run(&["session", "remove", "definitelyNotASession"]);
    assert_ne!(r.code, 0, "removing a missing session must fail: {r:?}");
    assert!(
        r.stderr.contains("does not exist"),
        "and it must be about the session, not about a cleanup step: {}",
        r.stderr
    );
    assert!(
        !r.stderr.contains("still running"),
        "no component can have survived a teardown that never ran: {}",
        r.stderr
    );
}

// ── Phase 4: persistent state ───────────────────────────────────────────────

/// A record that cannot be written means the operation's advertised state
/// cannot be established, so the command must not report success.
///
/// The record is what `session list`, `session status` and every session-scoped
/// verb read. A `stop` that printed `stopped` while the record still said
/// `Running` would be describing a state no later command would find.
#[test]
fn a_stop_whose_record_cannot_be_written_does_not_report_success() {
    runtime_dir();
    let sid = "cliunwritable";
    write_session_record(sid);

    // Replace the session directory with a file so writing the record back
    // fails for a reason no retry can fix. Deterministic: the failure is in the
    // filesystem, not in a race.
    let dir = maverickctl::session::session_dir(&SessionName::parse(sid).expect("name"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::write(&dir, b"not a directory").expect("replace dir with a file");

    let r = run(&["session", "stop", sid]);
    let _ = std::fs::remove_file(&dir);
    assert_ne!(
        r.code, 0,
        "a stop whose record could not be written must not report success: {r:?}"
    );
    assert!(
        !r.stdout.contains("stopped"),
        "no success line may be printed for a state that was never recorded: {}",
        r.stdout
    );
}
