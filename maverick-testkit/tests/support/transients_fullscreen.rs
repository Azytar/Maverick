use super::support::*;
use std::error::Error;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, GetGeometryReply, PropMode, Window};
use x11rb::wrapper::ConnectionExt as _;

fn transient(
    conn: &RustConnection,
    root: Window,
    parent: Window,
) -> Result<Window, Box<dyn Error>> {
    // Set the ICCCM relation before mapping; no float rule or IPC is required.
    let window = create_client(conn, root, false)?;
    conn.change_property32(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_TRANSIENT_FOR,
        AtomEnum::WINDOW,
        &[parent],
    )?
    .check()?;
    conn.map_window(window)?.check()?;
    conn.flush()?;
    Ok(window)
}

#[test]
#[ignore = "needs Xvfb and an already compiled release WM"]
fn transient_hierarchy_survives_focus_and_parent_destruction() -> Result<(), Box<dyn Error>> {
    for parent_first in [true, false] {
        let (mut session, conn, root) = start_session()?;
        let active = atom(&conn, b"_NET_ACTIVE_WINDOW")?;
        let clients = atom(&conn, b"_NET_CLIENT_LIST")?;
        let parent = map_client(&conn, root, true)?;
        let peer = map_client(&conn, root, true)?;
        let child = transient(&conn, root, parent)?;
        let grandchild = transient(&conn, root, child)?;
        let windows = [parent, peer, child, grandchild];
        wait_for(&mut session, "transient family registration", || {
            Ok(window_list(&conn, root, clients)?.len() == 4)
        })?;
        for target in [parent, child, grandchild, parent, peer, parent] {
            request_focus(&conn, root, active, target)?;
            wait_for(
                &mut session,
                "transient family stack and input focus",
                || {
                    let order = server_stack(&conn, root, &windows)?;
                    let above = |a, b| {
                        order.iter().position(|w| *w == a) > order.iter().position(|w| *w == b)
                    };
                    Ok(window_list(&conn, root, active)? == [target]
                        && conn.get_input_focus()?.reply()?.focus == target
                        && above(grandchild, child)
                        && above(child, parent)
                        && (target != peer || above(peer, grandchild)))
                },
            )?;
            assert_stacking(&conn, root, &windows)?;
        }
        let death_order = if parent_first {
            [parent, child, grandchild]
        } else {
            [grandchild, child, parent]
        };
        let mut live = windows.to_vec();
        for doomed in death_order {
            conn.destroy_window(doomed)?.check()?;
            conn.flush()?;
            live.retain(|w| *w != doomed);
            wait_for(&mut session, "family member cleanup", || {
                let list = window_list(&conn, root, clients)?;
                let stack = window_list(&conn, root, atom(&conn, b"_NET_CLIENT_LIST_STACKING")?)?;
                let focused = window_list(&conn, root, active)?;
                Ok(list.len() == live.len()
                    && live.iter().all(|w| list.contains(w))
                    && !stack.contains(&doomed)
                    && focused.iter().all(|w| live.contains(w)))
            })?;
            request_focus(&conn, root, active, peer)?;
            wait_for(&mut session, "surviving peer focus", || {
                Ok(window_list(&conn, root, active)? == [peer])
            })?;
            assert_stacking(&conn, root, &live)?;
        }
    }
    Ok(())
}

pub fn geometry(conn: &RustConnection, win: Window) -> Result<GetGeometryReply, Box<dyn Error>> {
    Ok(conn.get_geometry(win)?.reply()?)
}

#[test]
#[ignore = "needs Xvfb and an already compiled release WM"]
fn fullscreen_restores_geometry_and_obeys_float_dock_layers() -> Result<(), Box<dyn Error>> {
    for floating in [false, true] {
        let (mut session, conn, root) = start_session()?;
        let active = atom(&conn, b"_NET_ACTIVE_WINDOW")?;
        let owner = map_client(&conn, root, floating)?;
        wait_managed(&mut session, &conn, root, &[owner])?;
        let before = settled_geometry(&mut session, &conn, owner)?;
        set_state(&conn, root, owner, 1, b"_NET_WM_STATE_FULLSCREEN")?;
        wait_for(&mut session, "exclusive fullscreen geometry", || {
            let g = geometry(&conn, owner)?;
            Ok(has_state(&conn, owner, b"_NET_WM_STATE_FULLSCREEN")?
                && (g.x, g.y, g.width, g.height, g.border_width) == (0, 0, 800, 600, 0))
        })?;
        let tile = map_client(&conn, root, false)?;
        let float = map_client(&conn, root, true)?;
        let dock = create_client(&conn, root, false)?;
        conn.change_property32(
            PropMode::REPLACE,
            dock,
            atom(&conn, b"_NET_WM_WINDOW_TYPE")?,
            AtomEnum::ATOM,
            &[atom(&conn, b"_NET_WM_WINDOW_TYPE_DOCK")?],
        )?
        .check()?;
        conn.map_window(dock)?.check()?;
        conn.flush()?;
        wait_managed(&mut session, &conn, root, &[owner, tile, float])?;
        let windows = [owner, tile, float, dock];
        wait_for(&mut session, "dock stacking publication", || {
            Ok(
                window_list(&conn, root, atom(&conn, b"_NET_CLIENT_LIST_STACKING")?)?
                    .contains(&dock),
            )
        })?;
        for target in [owner, float, tile, owner] {
            request_focus(&conn, root, active, target)?;
            wait_for(&mut session, "fullscreen layer with float and dock", || {
                let order = server_stack(&conn, root, &windows)?;
                let top = owner;
                Ok(window_list(&conn, root, active)? == [owner] && order.last() == Some(&top))
            })?;
            assert_stacking(&conn, root, &windows)?;
        }
        // Remove idempotently. For a float the pre-fullscreen rect must return.
        for _ in 0..2 {
            set_state(&conn, root, owner, 0, b"_NET_WM_STATE_FULLSCREEN")?;
        }
        wait_for(
            &mut session,
            "fullscreen exit and restored dock layer",
            || {
                Ok(!has_state(&conn, owner, b"_NET_WM_STATE_FULLSCREEN")?
                    && server_stack(&conn, root, &windows)?.last() == Some(&dock))
            },
        )?;
        if floating {
            let after = settled_geometry(&mut session, &conn, owner)?;
            assert_eq!(
                (
                    before.x,
                    before.y,
                    before.width,
                    before.height,
                    before.border_width
                ),
                (
                    after.x,
                    after.y,
                    after.width,
                    after.height,
                    after.border_width
                )
            );
        }
        set_state(&conn, root, owner, 1, b"_NET_WM_STATE_FULLSCREEN")?;
        wait_for(&mut session, "fullscreen reentry", || {
            has_state(&conn, owner, b"_NET_WM_STATE_FULLSCREEN")
        })?;
        conn.destroy_window(owner)?.check()?;
        conn.flush()?;
        wait_for(&mut session, "fullscreen owner destruction", || {
            Ok(
                !window_list(&conn, root, atom(&conn, b"_NET_CLIENT_LIST")?)?.contains(&owner)
                    && server_stack(&conn, root, &[tile, float, dock])?.last() == Some(&dock),
            )
        })?;
        assert_stacking(&conn, root, &[tile, float, dock])?;
    }
    Ok(())
}

#[test]
#[ignore = "needs Xvfb and an already compiled release WM"]
fn deny_fullscreen_rule_refuses_client_requests() -> Result<(), Box<dyn Error>> {
    let (mut session, conn, root) = start_with_config(
        "[general]\nfocus_mouse=false\nwarp_cursor=false\n[autostart]\ncommands=[]\n[[rules]]\nwindow_type=\"dialog\"\ndeny_fullscreen=true\n")?;
    let window = map_client(&conn, root, true)?;
    wait_managed(&mut session, &conn, root, &[window])?;
    set_state(&conn, root, window, 1, b"_NET_WM_STATE_FULLSCREEN")?;
    // Attention is an ordered WM round-trip barrier, not a timing assumption.
    set_state(&conn, root, window, 1, b"_NET_WM_STATE_DEMANDS_ATTENTION")?;
    wait_for(&mut session, "denied request processed", || {
        has_state(&conn, window, b"_NET_WM_STATE_DEMANDS_ATTENTION")
    })?;
    assert!(!has_state(&conn, window, b"_NET_WM_STATE_FULLSCREEN")?);
    assert!(geometry(&conn, window)?.width < 800);
    Ok(())
}

#[test]
#[ignore = "needs Xvfb and an already compiled release WM"]
fn initial_ribbon_fullscreen_and_owned_popups_keep_their_layers() -> Result<(), Box<dyn Error>> {
    let (mut session, conn, root) = start_with_config(
        "[general]\nfocus_mouse=false\nwarp_cursor=false\nhonor_initial_state=true\n[autostart]\ncommands=[]\n")?;
    let owner = create_client(&conn, root, false)?;
    let fullscreen = atom(&conn, b"_NET_WM_STATE_FULLSCREEN")?;
    conn.change_property32(
        PropMode::REPLACE,
        owner,
        atom(&conn, b"_NET_WM_STATE")?,
        AtomEnum::ATOM,
        &[fullscreen],
    )?
    .check()?;
    conn.map_window(owner)?.check()?;
    conn.flush()?;
    wait_managed(&mut session, &conn, root, &[owner])?;
    wait_for(&mut session, "initial ribbon fullscreen", || {
        let g = geometry(&conn, owner)?;
        Ok(has_state(&conn, owner, b"_NET_WM_STATE_FULLSCREEN")?
            && g.width == 800
            && g.height == 600)
    })?;
    set_state(&conn, root, owner, 0, b"_NET_WM_STATE_FULLSCREEN")?;
    wait_for(&mut session, "leave initial ribbon fullscreen", || {
        Ok(!has_state(&conn, owner, b"_NET_WM_STATE_FULLSCREEN")?)
    })?;
    // An explicit client request promotes the owner to the exclusive overlay.
    set_state(&conn, root, owner, 1, b"_NET_WM_STATE_FULLSCREEN")?;
    wait_for(&mut session, "exclusive transition", || {
        has_state(&conn, owner, b"_NET_WM_STATE_FULLSCREEN")
    })?;
    let child = transient(&conn, root, owner)?;
    let grandchild = transient(&conn, root, child)?;
    wait_managed(&mut session, &conn, root, &[owner, child, grandchild])?;
    let active = atom(&conn, b"_NET_ACTIVE_WINDOW")?;
    request_focus(&conn, root, active, owner)?;
    wait_for(&mut session, "owned dialogs above focused overlay", || {
        Ok(window_list(&conn, root, active)? == [owner]
            && server_stack(&conn, root, &[owner, child, grandchild])?
                == [owner, child, grandchild])
    })?;
    assert_stacking(&conn, root, &[owner, child, grandchild])?;
    Ok(())
}
