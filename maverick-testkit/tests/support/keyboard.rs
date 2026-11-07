use super::support::*;
use std::error::Error;
use std::thread;
use std::time::Duration;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, KEY_PRESS_EVENT, KEY_RELEASE_EVENT};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

pub fn key(conn: &RustConnection, keysym: u32) -> Result<(), Box<dyn Error>> {
    let setup = conn.setup();
    let mapping = conn
        .get_keyboard_mapping(setup.min_keycode, setup.max_keycode - setup.min_keycode + 1)?
        .reply()?;
    let index = mapping
        .keysyms
        .chunks(usize::from(mapping.keysyms_per_keycode))
        .position(|symbols| symbols.first() == Some(&keysym))
        .ok_or("missing Xvfb keysym")?;
    let code = setup.min_keycode + u8::try_from(index)?;
    conn.xtest_fake_input(KEY_PRESS_EVENT, code, 0, x11rb::NONE, 0, 0, 0)?
        .check()?;
    conn.xtest_fake_input(KEY_RELEASE_EVENT, code, 0, x11rb::NONE, 0, 0, 0)?
        .check()?;
    conn.flush()?;
    // Real keybindings have a 60ms duplicate-key debounce. Model a human key.
    thread::sleep(Duration::from_millis(80));
    Ok(())
}

fn cardinal(conn: &RustConnection, root: u32, name: &[u8]) -> Result<Vec<u32>, Box<dyn Error>> {
    Ok(conn
        .get_property(false, root, atom(conn, name)?, AtomEnum::CARDINAL, 0, 64)?
        .reply()?
        .value32()
        .map(|v| v.collect())
        .unwrap_or_default())
}

#[test]
#[ignore = "needs Xvfb and an already compiled release WM"]
fn keyboard_views_and_carousel_publish_real_focus_and_desktop() -> Result<(), Box<dyn Error>> {
    let (mut session, conn, root) = start_with_config(
        "[general]\nn_tags=3\nfocus_mouse=false\nwarp_cursor=false\n[autostart]\ncommands=[]\n\
         [[keybindings]]\nkey=\"F1\"\naction=\"view_next\"\n\
         [[keybindings]]\nkey=\"F2\"\naction=\"view_prev\"\n\
         [[keybindings]]\nkey=\"F3\"\naction=\"view_return\"\n",
    )?;
    let active = atom(&conn, b"_NET_ACTIVE_WINDOW")?;
    let a = map_client(&conn, root, false)?;
    wait_managed(&mut session, &conn, root, &[a])?;
    key(&conn, 0xffbe)?;
    wait_for(&mut session, "next View", || {
        Ok(cardinal(&conn, root, b"_NET_CURRENT_DESKTOP")? == [1])
    })?;
    let b = map_client(&conn, root, false)?;
    wait_managed(&mut session, &conn, root, &[a, b])?;
    for (keysym, desktop, focused) in [(0xffbf, 0, a), (0xffbe, 1, b), (0xffc0, 0, a)] {
        key(&conn, keysym)?;
        wait_for(
            &mut session,
            "carousel desktop and focus restoration",
            || {
                Ok(cardinal(&conn, root, b"_NET_CURRENT_DESKTOP")? == [desktop]
                    && window_list(&conn, root, active)? == [focused]
                    && conn.get_input_focus()?.reply()?.focus == focused)
            },
        )?;
    }
    assert_eq!(cardinal(&conn, root, b"_NET_NUMBER_OF_DESKTOPS")?, [3]);
    assert_stacking(&conn, root, &[a, b])?;
    Ok(())
}

#[test]
#[ignore = "needs Xvfb and an already compiled release WM"]
fn keyboard_collapse_and_vertical_focus_follow_real_geometry() -> Result<(), Box<dyn Error>> {
    let (mut session, conn, root) = start_with_config(
        "[general]\nfocus_mouse=false\nwarp_cursor=false\n[autostart]\ncommands=[]\n\
         [[keybindings]]\nkey=\"F1\"\naction=\"collapse_column\"\n\
         [[keybindings]]\nkey=\"F2\"\naction=\"focus:up\"\n\
         [[keybindings]]\nkey=\"F3\"\naction=\"focus:down\"\n",
    )?;
    let a = map_client(&conn, root, false)?;
    wait_managed(&mut session, &conn, root, &[a])?;
    let b = map_client(&conn, root, false)?;
    wait_managed(&mut session, &conn, root, &[a, b])?;
    key(&conn, 0xffbe)?;
    wait_for(&mut session, "collapsed column geometry", || {
        let ga = conn.get_geometry(a)?.reply()?;
        let gb = conn.get_geometry(b)?.reply()?;
        Ok(ga.x == gb.x
            && ga.width == gb.width
            && ga.y < gb.y
            && ga.height < 600
            && gb.height < 600)
    })?;
    let active = atom(&conn, b"_NET_ACTIVE_WINDOW")?;
    for (keysym, focused) in [(0xffbf, a), (0xffc0, b)] {
        key(&conn, keysym)?;
        wait_for(&mut session, "vertical keyboard focus", || {
            Ok(window_list(&conn, root, active)? == [focused]
                && conn.get_input_focus()?.reply()?.focus == focused)
        })?;
    }
    Ok(())
}

#[test]
#[ignore = "needs Xvfb and an already compiled release WM"]
fn keyboard_overview_preserves_client_geometry_and_mapping() -> Result<(), Box<dyn Error>> {
    let (mut session, conn, root) = start_with_config(
        "[general]\nfocus_mouse=false\nwarp_cursor=false\n[autostart]\ncommands=[]\n\
         [[keybindings]]\nkey=\"F1\"\naction=\"toggle_overview\"\n",
    )?;
    let window = map_client(&conn, root, false)?;
    wait_managed(&mut session, &conn, root, &[window])?;
    let before = settled_geometry(&mut session, &conn, window)?;
    let visible_overview = || -> Result<bool, Box<dyn Error>> {
        for child in conn.query_tree(root)?.reply()?.children {
            let name = conn
                .get_property(false, child, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 64)?
                .reply()?;
            if name.value == b"Maverick Overview"
                && conn.get_window_attributes(child)?.reply()?.map_state
                    == x11rb::protocol::xproto::MapState::VIEWABLE
            {
                return Ok(true);
            }
        }
        Ok(false)
    };
    for showing in [true, false, true, false] {
        key(&conn, 0xffbe)?;
        wait_for(&mut session, "Overview visibility", || {
            Ok(visible_overview()? == showing)
        })?;
        let after = settled_geometry(&mut session, &conn, window)?;
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
        assert_eq!(
            conn.get_window_attributes(window)?.reply()?.map_state,
            x11rb::protocol::xproto::MapState::VIEWABLE
        );
    }
    Ok(())
}

#[test]
#[ignore = "needs Xvfb and an already compiled release WM"]
fn keyboard_restart_restores_live_clients_focus_and_geometry() -> Result<(), Box<dyn Error>> {
    let (mut session, conn, root) = start_with_config(
        "[general]\nfocus_mouse=false\nwarp_cursor=false\n[autostart]\ncommands=[]\n\
         [[keybindings]]\nkey=\"F1\"\naction=\"restart\"\n",
    )?;
    let a = map_client(&conn, root, false)?;
    wait_managed(&mut session, &conn, root, &[a])?;
    let b = map_client(&conn, root, false)?;
    wait_managed(&mut session, &conn, root, &[a, b])?;
    let float = map_client(&conn, root, true)?;
    wait_managed(&mut session, &conn, root, &[a, b, float])?;
    let mut before = Vec::new();
    for window in [a, b, float] {
        let g = settled_geometry(&mut session, &conn, window)?;
        before.push((g.x, g.y, g.width, g.height, g.border_width));
    }
    let active = atom(&conn, b"_NET_ACTIVE_WINDOW")?;
    let focused = window_list(&conn, root, active)?;
    let log = session.wm_log_path();
    for attempt in 0..2 {
        key(&conn, 0xffbe)?;
        wait_for(&mut session, "restart handshake completion", || {
            Ok(std::fs::read_to_string(&log)?
                .matches("maverick ready")
                .count()
                >= attempt + 2)
        })?;
        wait_managed(&mut session, &conn, root, &[a, b, float])?;
        for (index, window) in [a, b, float].into_iter().enumerate() {
            let g = settled_geometry(&mut session, &conn, window)?;
            assert_eq!((g.x, g.y, g.width, g.height, g.border_width), before[index]);
        }
        assert_eq!(window_list(&conn, root, active)?, focused);
        assert_eq!(conn.get_input_focus()?.reply()?.focus, focused[0]);
        assert_stacking(&conn, root, &[a, b, float])?;
    }
    Ok(())
}
