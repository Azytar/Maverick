//! Properties of the client half of the control protocol.
//!
//! `send_command` and its wrappers are the only place Maverick turns a
//! caller-supplied string into bytes on the control socket, and the protocol it
//! feeds is line framed. Two things have to hold for every argv the CLI can
//! produce: a payload that could terminate the line early must be refused
//! *before* anything is sent, and a session id that could escape the runtime
//! directory must be refused before the socket layer is even reached.

mod common;

use maverick_sys::control::{identity_json, MAX_CMD_LEN};
use maverick_sys::identity::{is_valid_sid, InstanceInfo, PING_CMD};
use maverick_sys::json::json_quote;
use maverickctl::client::{dispatch, query, send_command};
use proptest::prelude::*;
use std::io::ErrorKind;

/// A session id nobody is running, so a payload that gets past validation
/// cannot reach a real window manager.
const NO_INSTANCE: &str = "propsid";

/// Session ids that `is_valid_sid` has to refuse: a separator, a dot, a NUL, or
/// simply too long. Every branch is built around a valid prefix so the
/// rejection can only come from the tail.
fn traversal_sid() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => prop::sample::select(vec!["/", "..", ".", "\u{0}", "a/b", "x/", "-/"]).prop_map(String::from),
        1 => "[A-Za-z0-9_-]{65,70}",
    ]
    .prop_map(|tail: String| format!("a{tail}"))
}

// A CR or LF inside a command would end the protocol line early and turn the
// rest of the caller's string into a second command. The module documents that
// such payloads are rejected, and the rejection has to happen before the
// connection is made — a payload the caller believes was never sent must not
// reach the WM.
proptest! {
    #[test]
    fn send_command_refuses_embedded_newlines(
        head in "[ -~]{0,40}",
        eol in prop_oneof![Just('\n'), Just('\r')],
        tail in "[ -~]{0,40}",
    ) {
        let cmd = format!("{head}{eol}{tail}");
        let err = send_command(NO_INSTANCE, &cmd).expect_err("an embedded line break must be refused");
        prop_assert_eq!(err.kind(), ErrorKind::InvalidInput);
        prop_assert!(err.to_string().contains("newline"), "refusal must name the reason: {err}");
    }
}

// Command length is bounded so a caller cannot make the server buffer without
// limit. The bound is inclusive: a command of exactly `MAX_CMD_LEN` is a legal
// one and must be refused, if at all, by the socket — never by the length check.
proptest! {
    #[test]
    fn send_command_bounds_the_command_length(over in 1usize..8) {
        let err = send_command(NO_INSTANCE, &"x".repeat(MAX_CMD_LEN + over))
            .expect_err("an over-long command must be refused");
        prop_assert_eq!(err.kind(), ErrorKind::InvalidInput);
        prop_assert!(err.to_string().contains("too long"), "refusal must name the reason: {err}");

        let at_bound = send_command(NO_INSTANCE, &"x".repeat(MAX_CMD_LEN))
            .expect_err("nothing is listening on the test session id");
        prop_assert!(!at_bound.to_string().contains("too long"), "the bound itself must stay legal");
    }
}

// The session id is validated before the filesystem is touched, so a traversal
// attempt is refused on its own terms rather than surfacing as whatever the
// socket layer happens to report.
proptest! {
    #[test]
    fn send_command_refuses_traversal_before_connecting(sid in traversal_sid()) {
        prop_assert!(!is_valid_sid(&sid), "generator must produce an unusable id, got {:?}", sid);
        let err = send_command(&sid, PING_CMD).expect_err("a traversal sid must never reach a socket");
        prop_assert_eq!(err.kind(), ErrorKind::InvalidInput);
        prop_assert!(err.to_string().contains("session id"), "refusal must name the reason: {err}");
    }
}

// `dispatch`/`query` splice the payload into a protocol line themselves, so
// they have to carry the same refusal as the raw command path: an action
// containing a line break must not become a second command.
proptest! {
    #[test]
    fn dispatch_and_query_refuse_payloads_that_could_inject_a_command(
        head in "[a-z-]{0,12}",
        eol in prop_oneof![Just('\n'), Just('\r')],
        tail in "[a-z-]{0,12}",
    ) {
        let payload = format!("{head}{eol}{tail}");
        for err in [
            dispatch(NO_INSTANCE, &payload).expect_err("a dispatch payload must be refused"),
            query(NO_INSTANCE, &payload).expect_err("a query topic must be refused"),
        ] {
            prop_assert_eq!(err.kind(), ErrorKind::InvalidInput);
            prop_assert!(err.to_string().contains("newline"), "refusal must name the reason: {err}");
        }
    }
}

// The `identify` reply is this JSON document plus a newline, read back with a
// single `read_line`: it has to stay on one line however hostile the instance
// name or executable path are, and every string field has to travel through the
// shared escaper so the reader can recover it.
proptest! {
    #[test]
    fn identity_json_stays_a_single_escaped_line(
        name in common::text(),
        display in common::text(),
        x_server_identity in common::text(),
        exe in common::text(),
        session_id in "[A-Za-z0-9_-]{1,64}",
        pid in any::<u32>(),
        tty_nr in any::<u64>(),
        start_time in any::<u64>(),
        started_at in any::<u64>(),
        alive in any::<bool>(),
    ) {
        let info = InstanceInfo {
            name: name.clone(),
            session_id: session_id.clone(),
            pid,
            display: display.clone(),
            tty_nr,
            x_server_identity: x_server_identity.clone(),
            start_time,
            exe: exe.clone(),
            started_at,
            alive,
        };
        let json = identity_json(&info);
        prop_assert!(json.starts_with('{') && json.ends_with('}'), "not a JSON object: {json:?}");
        prop_assert!(!json.contains(['\n', '\r']), "reply would break line framing: {json:?}");
        for field in [&name, &display, &x_server_identity, &exe, &session_id] {
            prop_assert!(
                json.contains(&json_quote(field)),
                "field {:?} was not carried through the shared escaper",
                field
            );
        }
        prop_assert!(json.contains(&pid.to_string()), "pid must survive as a plain number");
        prop_assert!(json.contains(&tty_nr.to_string()), "tty_nr must survive as a plain number");
    }
}
