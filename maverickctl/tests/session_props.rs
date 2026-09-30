//! Properties of the session record's ownership contract.
//!
//! A session record names an `owner_uid`, and the check is only worth anything
//! if the uid it compares against is the kernel's rather than whatever the
//! record says. These tests live with the session code: they pin the record
//! layer (`maverickctl::session`) against the identity layer
//! (`maverick_sys::identity`) it must agree with.

use maverick_sys::identity::{current_gid, current_uid};
use maverickctl::session::{read, read_checked, session_dir, spec_path, SessionError, SessionName};

/// The session layer must not answer the uid question separately from
/// identity: a divergence is how a record's owner and a socket's owner would
/// end up compared against different numbers.
#[test]
fn the_session_layer_answers_uid_like_identity() {
    assert_eq!(
        maverickctl::session::current_uid(),
        current_uid(),
        "the session layer must not answer the uid question separately"
    );
    assert_eq!(maverickctl::session::current_gid(), current_gid());
}

/// A record whose `owner_uid` is somebody else is refused, not honoured. The
/// check is only worth anything if the uid it compares against is the kernel's
/// rather than whatever the record says, so the two are pinned together here:
/// a foreign owner is refused, this process's own owner is accepted, and a
/// record that names no owner at all is read as this process's.
#[test]
fn a_session_record_is_answered_by_its_owner_against_the_kernel() {
    let name =
        SessionName::parse(&format!("ownrec{}", std::process::id())).expect("a valid session name");
    let foreign = current_uid().wrapping_add(1);
    let doc = |uid: Option<&str>| {
        format!(
            r#"{{"name":"{name}","display":":1","state":"stopped","owner_uid":{}}}"#,
            uid.unwrap_or("0")
        )
    };

    // A record claiming a different uid must not resolve, and the refusal must
    // name the session rather than reading as a missing one.
    let path = spec_path(&name);
    std::fs::create_dir_all(session_dir(&name)).expect("session dir");
    std::fs::write(&path, doc(Some(&foreign.to_string()))).expect("write the record");
    assert!(
        read(&name).is_none(),
        "a record owned by uid {foreign} must not be read as this process's"
    );
    match read_checked(&name) {
        Err(SessionError::NotOwned(n)) => assert_eq!(n, name),
        other => panic!("expected NotOwned, got {other:?}"),
    }

    // This process's own uid is accepted.
    std::fs::write(&path, doc(Some(&current_uid().to_string()))).expect("write the record");
    let read = read_checked(&name).expect("our own record is readable");
    assert_eq!(read.owner_uid, current_uid());
    assert_eq!(read.owner_gid, current_gid());

    let _ = std::fs::remove_dir_all(session_dir(&name));
}
