//! Properties of the session-id contract in `identity`.
//!
//! A session id is the only untrusted string Maverick ever turns into a path:
//! it arrives from a `--name`, from `$MAVERICK_INSTANCE`, from a CLI flag and
//! from a directory listing under the runtime dir. Everything downstream — the
//! `0700` session directory, the control socket, the identity ficha — is keyed
//! by it, so an id that is not a single safe path component is a traversal and
//! a cross-session collision waiting to happen.

use maverick_sys::identity::{
    is_valid_sid, meta_path, new_session_id, session_dir, sock_path, try_meta_path,
    try_session_dir, try_sock_path, MAX_SID_LEN,
};
use proptest::prelude::*;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// `sockaddr_un.sun_path` is 108 bytes on Linux; a longer path cannot be bound.
const SUN_LEN: usize = 108;

/// Ids as external input actually produces them: the accepted charset across
/// the whole documented length range, the exact `MAX_SID_LEN` boundary, and the
/// traversal shapes a hostile or careless caller would try.
fn sid_candidate() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => "[A-Za-z0-9_-]{0,80}",
        2 => "[A-Za-z0-9_-]{60,70}",
        1 => any::<String>(),
        1 => prop::sample::select(vec![
            "", ".", "..", "/", "../..", "a/b", "a.b", ".hidden", "-", "_", " ", "a b", "a\0b",
            "\u{0}", "\u{e9}", "\u{ff21}\u{ff22}", "a\nb", "a\tb", "0", "--", "__",
        ]).prop_map(String::from),
    ]
}

/// Path components below `root`, as plain strings, so an assertion about "the
/// sid is spent exactly once" does not depend on where the runtime dir sits.
fn tail_components(path: &Path, root: &Path) -> Vec<String> {
    path.strip_prefix(root)
        .expect("path is expected to live under the runtime dir")
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect()
}

fn runtime_root() -> PathBuf {
    maverick_sys::identity::runtime_dir()
}

// The validator documents one rule: non-empty, at most `MAX_SID_LEN` bytes, and
// only `[A-Za-z0-9_-]`. The `.`/`..` literals the implementation also rejects
// are already outside that charset, so the rule above is the independent
// statement of the contract — including the length bound, which is the only
// thing keeping `sun_path` inside `SUN_LEN`.
proptest! {
    #[test]
    fn is_valid_sid_matches_its_documented_charset(sid in sid_candidate()) {
        let by_contract = !sid.is_empty()
            && sid.len() <= MAX_SID_LEN
            && sid
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        prop_assert_eq!(is_valid_sid(&sid), by_contract, "verdict on {:?} left the documented rule", sid);
    }
}

// An accepted id must be usable as a single path component: both derived paths
// hang off the runtime dir, and the ficha — whose *filename* embeds the id —
// must be resolved through the validated session dir, never through the runtime
// dir directly. That indirection is the traversal the validator exists for.
proptest! {
    #[test]
    fn accepted_sids_stay_inside_the_runtime_dir(sid in sid_candidate()) {
        prop_assume!(is_valid_sid(&sid));
        let root = runtime_root();
        let dir = try_session_dir(&sid).expect("an accepted sid must resolve");
        prop_assert_eq!(dir.parent(), Some(root.as_path()));
        prop_assert_eq!(dir.file_name().and_then(|s| s.to_str()), Some(sid.as_str()));

        let meta = try_meta_path(&sid).expect("an accepted sid must resolve");
        prop_assert_eq!(meta.parent(), Some(dir.as_path()), "ficha escaped its session dir");
        let expected_name = format!("{sid}.json");
        prop_assert_eq!(meta.file_name().and_then(|s| s.to_str()), Some(expected_name.as_str()));
    }
}

// The socket path deliberately spends the random id once, as the directory
// name, under a fixed `control.sock` filename: spending it twice (directory
// *and* `<sid>.sock`) is what used to push the path past the kernel's
// `sun_path` budget and make `bind` fail.
proptest! {
    #[test]
    fn socket_path_spends_the_sid_exactly_once(sid in sid_candidate()) {
        prop_assume!(is_valid_sid(&sid));
        if let Ok(path) = try_sock_path(&sid) {
            prop_assert_eq!(
                tail_components(&path, &runtime_root()),
                vec![sid.clone(), "control.sock".to_string()],
                "socket path must be <runtime>/<sid>/control.sock"
            );
            prop_assert!(
                path.as_os_str().len() < SUN_LEN,
                "socket path is {} bytes, past SUN_LEN",
                path.as_os_str().len()
            );
        }
    }
}

// A rejected id must fail the same way everywhere, before any path is built:
// `InvalidInput` from the fallible helpers, and the documented `__invalid__`
// sentinel from the infallible ones, so a caller that ignores the error still
// cannot end up acting on a path built from the rejected id.
proptest! {
    #[test]
    fn rejected_sids_are_refused_before_touching_the_filesystem(sid in sid_candidate()) {
        prop_assume!(!is_valid_sid(&sid));
        for resolved in [try_session_dir(&sid), try_meta_path(&sid), try_sock_path(&sid)] {
            let err = resolved.expect_err("a rejected sid must not resolve to a path");
            prop_assert_eq!(err.kind(), ErrorKind::InvalidInput);
            prop_assert!(!err.to_string().is_empty(), "the refusal must say why");
        }
        let root = runtime_root();
        let sentinel = root.join("__invalid__");
        let invalid_sock = sentinel.join("control.sock");
        prop_assert_eq!(&session_dir(&sid), &sentinel);
        prop_assert_eq!(meta_path(&sid), root.join("__invalid__.json"));
        prop_assert_eq!(sock_path(&sid), invalid_sock);
    }
}

// The generator is the other half of the contract: an id it produces is used as
// a directory name, a socket name and a ficha name without further checking, so
// it has to satisfy the validator and leave room under `SUN_LEN` on its own.
proptest! {
    #[test]
    fn generated_session_ids_are_always_usable(samples in 1u8..4) {
        for _ in 0..samples {
            let sid = new_session_id();
            prop_assert!(is_valid_sid(&sid), "generated id {sid:?} is not a valid session id");
            prop_assert!(sid.len() <= MAX_SID_LEN, "generated id {sid:?} is over the documented bound");
            if let Ok(path) = try_sock_path(&sid) {
                prop_assert!(
                    path.as_os_str().len() < SUN_LEN,
                    "generated id {sid:?} yields a {}-byte socket path",
                    path.as_os_str().len()
                );
            }
        }
    }
}
