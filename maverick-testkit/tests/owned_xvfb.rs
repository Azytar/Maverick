//! Protocol and lifecycle regressions against an owned Xvfb and release WM.
#![forbid(unsafe_code)]

#[path = "support/failures.rs"]
mod failures;
#[path = "support/keyboard.rs"]
mod keyboard;

#[path = "support/input_hints.rs"]
mod input_hints;
mod support;
#[path = "support/transients_fullscreen.rs"]
mod transients_fullscreen;

use std::error::Error;
use support::*;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt;

#[test]
#[ignore = "needs Xvfb and an already compiled release WM"]
fn managed_windows_follow_real_server_stacking() -> Result<(), Box<dyn Error>> {
    let (mut session, conn, root) = start_session()?;
    let client_list = atom(&conn, b"_NET_CLIENT_LIST")?;
    let stacking = atom(&conn, b"_NET_CLIENT_LIST_STACKING")?;
    let windows = [
        map_client(&conn, root, false)?,
        map_client(&conn, root, false)?,
    ];

    wait_for(&mut session, "both managed windows", || {
        let tracked = window_list(&conn, root, client_list)?;
        Ok(windows.iter().all(|window| tracked.contains(window)))
    })?;

    wait_for(&mut session, "both stacking entries", || {
        let published: Vec<_> = window_list(&conn, root, stacking)?
            .into_iter()
            .filter(|window| windows.contains(window))
            .collect();
        Ok(published.len() == windows.len())
    })?;
    assert_stacking(&conn, root, &windows)?;
    Ok(())
}

#[test]
#[ignore = "needs Xvfb and an already compiled release WM"]
fn focus_requests_raise_real_floating_clients() -> Result<(), Box<dyn Error>> {
    let (mut session, conn, root) = start_session()?;
    let active = atom(&conn, b"_NET_ACTIVE_WINDOW")?;
    let client_list = atom(&conn, b"_NET_CLIENT_LIST")?;
    let stacking = atom(&conn, b"_NET_CLIENT_LIST_STACKING")?;
    let windows = [
        map_client(&conn, root, true)?,
        map_client(&conn, root, true)?,
    ];

    wait_for(&mut session, "both floating windows", || {
        let tracked = window_list(&conn, root, client_list)?;
        Ok(windows.iter().all(|window| tracked.contains(window)))
    })?;

    for target in [windows[0], windows[1], windows[0]] {
        request_focus(&conn, root, active, target)?;

        wait_for(&mut session, "EWMH focus and floating stack", || {
            let focused = window_list(&conn, root, active)?;
            let order: Vec<_> = window_list(&conn, root, stacking)?
                .into_iter()
                .filter(|window| windows.contains(window))
                .collect();
            Ok(focused.first() == Some(&target)
                && order.len() == windows.len()
                && order.last() == Some(&target))
        })?;
        assert_eq!(
            assert_stacking(&conn, root, &windows)?.last(),
            Some(&target)
        );
    }
    Ok(())
}

#[test]
#[ignore = "needs Xvfb and an already compiled release WM"]
fn destroying_focused_client_restores_focus_and_cleans_ewmh() -> Result<(), Box<dyn Error>> {
    let (mut session, conn, root) = start_session()?;
    let active = atom(&conn, b"_NET_ACTIVE_WINDOW")?;
    let client_list = atom(&conn, b"_NET_CLIENT_LIST")?;
    let stacking = atom(&conn, b"_NET_CLIENT_LIST_STACKING")?;
    let survivor = map_client(&conn, root, false)?;
    let doomed = map_client(&conn, root, true)?;

    wait_for(&mut session, "both managed clients", || {
        let clients = window_list(&conn, root, client_list)?;
        Ok(clients.contains(&survivor) && clients.contains(&doomed))
    })?;
    request_focus(&conn, root, active, doomed)?;
    wait_for(&mut session, "focused floating client", || {
        Ok(window_list(&conn, root, active)? == [doomed])
    })?;

    conn.destroy_window(doomed)?.check()?;
    conn.flush()?;
    wait_for(
        &mut session,
        "destroyed client removal and focus fallback",
        || {
            let clients = window_list(&conn, root, client_list)?;
            let order = window_list(&conn, root, stacking)?;
            Ok(clients == [survivor]
                && order == [survivor]
                && window_list(&conn, root, active)? == [survivor])
        },
    )?;
    assert_stacking(&conn, root, &[survivor])?;

    conn.destroy_window(survivor)?.check()?;
    conn.flush()?;
    wait_for(&mut session, "last client teardown", || {
        Ok(window_list(&conn, root, client_list)?.is_empty()
            && window_list(&conn, root, stacking)?.is_empty()
            && window_list(&conn, root, active)? == [x11rb::NONE])
    })?;
    Ok(())
}

#[test]
#[ignore = "needs Xvfb and an already compiled release WM"]
fn withdrawn_client_can_remap_without_stale_state() -> Result<(), Box<dyn Error>> {
    let (mut session, conn, root) = start_session()?;
    let active = atom(&conn, b"_NET_ACTIVE_WINDOW")?;
    let client_list = atom(&conn, b"_NET_CLIENT_LIST")?;
    let stacking = atom(&conn, b"_NET_CLIENT_LIST_STACKING")?;
    let window = map_client(&conn, root, true)?;

    wait_for(&mut session, "mapped floating client", || {
        Ok(window_list(&conn, root, client_list)? == [window])
    })?;
    request_focus(&conn, root, active, window)?;
    wait_for(&mut session, "initial client focus", || {
        Ok(window_list(&conn, root, active)? == [window])
    })?;

    // UnmapNotify leaves the XID alive, but must remove every WM reference.
    conn.unmap_window(window)?.check()?;
    conn.flush()?;
    wait_for(&mut session, "withdrawn client removal", || {
        Ok(window_list(&conn, root, client_list)?.is_empty()
            && window_list(&conn, root, stacking)?.is_empty()
            && window_list(&conn, root, active)? == [x11rb::NONE])
    })?;
    assert!(
        conn.query_tree(root)?.reply()?.children.contains(&window),
        "unmapping must not destroy the client XID"
    );

    // Mapping the same XID must be treated as a fresh managed window.
    conn.map_window(window)?.check()?;
    conn.flush()?;
    wait_for(&mut session, "remapped client registration", || {
        Ok(window_list(&conn, root, client_list)? == [window]
            && window_list(&conn, root, stacking)? == [window])
    })?;
    request_focus(&conn, root, active, window)?;
    wait_for(&mut session, "remapped client focus", || {
        Ok(window_list(&conn, root, active)? == [window])
    })?;
    assert_stacking(&conn, root, &[window])?;
    Ok(())
}
