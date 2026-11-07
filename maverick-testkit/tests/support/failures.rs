use super::support::*;
use std::error::Error;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, PropMode};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

#[test]
#[ignore = "needs Xvfb and an already compiled release WM"]
fn stale_focus_and_duplicate_state_messages_preserve_survivor() -> Result<(), Box<dyn Error>> {
    let (mut session, conn, root) = start_session()?;
    let active = atom(&conn, b"_NET_ACTIVE_WINDOW")?;
    let survivor = map_client(&conn, root, false)?;
    let doomed = map_client(&conn, root, true)?;
    wait_managed(&mut session, &conn, root, &[survivor, doomed])?;
    conn.unmap_window(doomed)?.check()?;
    conn.destroy_window(doomed)?.check()?;
    conn.flush()?;
    wait_for(&mut session, "unmap/destroy coalescing", || {
        Ok(window_list(&conn, root, atom(&conn, b"_NET_CLIENT_LIST")?)? == [survivor])
    })?;
    for _ in 0..8 {
        request_focus(&conn, root, active, doomed)?;
        set_state(&conn, root, survivor, 99, b"_NET_WM_STATE_FULLSCREEN")?;
        set_state(&conn, root, survivor, 0, b"_NET_WM_STATE_FULLSCREEN")?;
    }
    set_state(&conn, root, survivor, 1, b"_NET_WM_STATE_DEMANDS_ATTENTION")?;
    wait_for(&mut session, "invalid requests drained", || {
        has_state(&conn, survivor, b"_NET_WM_STATE_DEMANDS_ATTENTION")
    })?;
    assert!(!has_state(&conn, survivor, b"_NET_WM_STATE_FULLSCREEN")?);
    assert_eq!(window_list(&conn, root, active)?, [survivor]);
    assert_eq!(conn.get_input_focus()?.reply()?.focus, survivor);
    assert_stacking(&conn, root, &[survivor])?;
    Ok(())
}

#[test]
#[ignore = "needs Xvfb and an already compiled release WM"]
fn malformed_optional_properties_do_not_prevent_management() -> Result<(), Box<dyn Error>> {
    let (mut session, conn, root) = start_session()?;
    let window = create_client(&conn, root, false)?;
    for property in [
        atom(&conn, b"_NET_WM_STATE")?,
        AtomEnum::WM_TRANSIENT_FOR.into(),
        AtomEnum::WM_HINTS.into(),
    ] {
        conn.change_property8(
            PropMode::REPLACE,
            window,
            property,
            AtomEnum::STRING,
            b"malformed",
        )?
        .check()?;
    }
    conn.map_window(window)?.check()?;
    conn.flush()?;
    wait_managed(&mut session, &conn, root, &[window])?;
    let active = atom(&conn, b"_NET_ACTIVE_WINDOW")?;
    request_focus(&conn, root, active, window)?;
    wait_for(&mut session, "malformed-hint client focus", || {
        Ok(window_list(&conn, root, active)? == [window])
    })?;
    assert_stacking(&conn, root, &[window])?;
    Ok(())
}

#[test]
#[ignore = "needs Xvfb and an already compiled release WM"]
fn disconnected_client_is_removed_without_affecting_other_connections() -> Result<(), Box<dyn Error>>
{
    let (mut session, conn, root) = start_session()?;
    let survivor = map_client(&conn, root, false)?;
    let (stream, _) =
        x11rb::rust_connection::DefaultStream::from_unix_stream(session.connect_stream()?)?;
    let other = RustConnection::connect_to_stream(stream, 0)?;
    let window = map_client(&other, root, true)?;
    wait_managed(&mut session, &conn, root, &[survivor, window])?;
    drop(other);
    wait_for(&mut session, "disconnected client cleanup", || {
        Ok(
            window_list(&conn, root, atom(&conn, b"_NET_CLIENT_LIST")?)? == [survivor]
                && window_list(&conn, root, atom(&conn, b"_NET_CLIENT_LIST_STACKING")?)?
                    == [survivor],
        )
    })?;
    assert_stacking(&conn, root, &[survivor])?;
    Ok(())
}

#[test]
#[ignore = "needs Xvfb and an already compiled release WM"]
fn fixture_reports_owned_wm_and_server_failure() -> Result<(), Box<dyn Error>> {
    let (mut session, _conn, _) = start_session()?;
    // A missing executable must fail at spawn rather than silently falling back.
    session.stop_wm();
    assert!(session
        .launch_wm(std::path::Path::new("/nonexistent/maverick-testkit-binary"))
        .is_err());
    session.launch_wm(&release_binary())?;
    session.stop_server()?;
    let error = wait_for(&mut session, "deliberately stopped server", || Ok(false)).unwrap_err();
    assert!(error.to_string().contains("Xvfb exited"), "{error}");
    Ok(())
}
