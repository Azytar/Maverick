use super::keyboard::key;
use super::support::*;
use maverick_testkit::XvfbSession;
use std::error::Error;
use std::thread;
use std::time::{Duration, Instant};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, PropMode, Window};
use x11rb::wrapper::ConnectionExt as _;

fn hints(
    conn: &RustConnection,
    window: Window,
    declaration: Option<(u32, u32)>,
) -> Result<(), Box<dyn Error>> {
    if let Some((flags, input)) = declaration {
        conn.change_property32(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_HINTS,
            AtomEnum::WM_HINTS,
            &[flags, input, 0, 0, 0, 0, 0, 0, 0],
        )?
        .check()?;
    } else {
        conn.delete_property(window, AtomEnum::WM_HINTS.into())?
            .check()?;
    }
    conn.flush()?;
    Ok(())
}

// Refusals must stay refused after the event loop processes the request.
// A successful first poll is insufficient for a negative focus assertion.
fn observe_focus(
    session: &mut XvfbSession,
    conn: &RustConnection,
    root: Window,
    target: Window,
    forbidden: bool,
) -> Result<(), Box<dyn Error>> {
    let active = atom(conn, b"_NET_ACTIVE_WINDOW")?;
    let matches = || -> Result<bool, Box<dyn Error>> {
        let published = window_list(conn, root, active)?;
        let real = conn.get_input_focus()?.reply()?.focus;
        Ok(if forbidden {
            !published.contains(&target) && real != target
        } else {
            published == [target] && real == target
        })
    };
    if !forbidden {
        wait_for(session, "eligible client focus", matches)?;
    }
    let until = Instant::now() + Duration::from_millis(600);
    while Instant::now() < until {
        if !matches()? || session.wm_exit_status()?.is_some() {
            return Err(format!(
                "unstable input eligibility for {target:#x}, forbidden={forbidden}\n{}",
                session.diagnostics()
            )
            .into());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

#[test]
#[ignore = "needs Xvfb and an already compiled release WM"]
fn input_hint_changes_remap_and_keyboard_obey_current_eligibility() -> Result<(), Box<dyn Error>> {
    let (mut session, conn, root) = start_with_config(
        "[general]\nfocus_mouse=false\nwarp_cursor=false\n[autostart]\ncommands=[]\n\
         [[keybindings]]\nkey=\"F1\"\naction=\"focus:left\"\n",
    )?;
    let active = atom(&conn, b"_NET_ACTIVE_WINDOW")?;
    let a = create_client(&conn, root, false)?;
    let b = create_client(&conn, root, false)?;
    for window in [a, b] {
        hints(&conn, window, Some((1, 0)))?;
        conn.map_window(window)?.check()?;
        conn.flush()?;
        wait_managed(&mut session, &conn, root, &[window])?;
    }
    request_focus(&conn, root, active, a)?;
    observe_focus(&mut session, &conn, root, a, true)?;
    for window in [a, b] {
        hints(&conn, window, Some((1, 1)))?;
        request_focus(&conn, root, active, window)?;
        observe_focus(&mut session, &conn, root, window, false)?;
    }
    hints(&conn, a, Some((1, 0)))?;
    request_focus(&conn, root, active, a)?;
    observe_focus(&mut session, &conn, root, b, false)?;
    key(&conn, 0xffbe)?;
    observe_focus(&mut session, &conn, root, a, true)?;
    request_focus(&conn, root, active, b)?;
    observe_focus(&mut session, &conn, root, b, false)?;
    hints(&conn, a, Some((1, 1)))?;
    request_focus(&conn, root, active, a)?;
    observe_focus(&mut session, &conn, root, a, false)?;
    for declaration in [None, Some((0, 0))] {
        hints(&conn, b, declaration)?;
        request_focus(&conn, root, active, a)?;
        observe_focus(&mut session, &conn, root, a, false)?;
        request_focus(&conn, root, active, b)?;
        observe_focus(&mut session, &conn, root, b, false)?;
    }
    hints(&conn, a, Some((1, 0)))?;
    conn.unmap_window(a)?.check()?;
    conn.flush()?;
    wait_for(&mut session, "input-hint client withdrawn", || {
        Ok(window_list(&conn, root, atom(&conn, b"_NET_CLIENT_LIST")?)? == [b])
    })?;
    conn.map_window(a)?.check()?;
    conn.flush()?;
    wait_managed(&mut session, &conn, root, &[a, b])?;
    request_focus(&conn, root, active, a)?;
    observe_focus(&mut session, &conn, root, a, true)?;
    hints(&conn, a, Some((1, 1)))?;
    request_focus(&conn, root, active, a)?;
    observe_focus(&mut session, &conn, root, a, false)?;
    request_focus(&conn, root, active, b)?;
    observe_focus(&mut session, &conn, root, b, false)?;
    conn.destroy_window(b)?.check()?;
    conn.flush()?;
    observe_focus(&mut session, &conn, root, a, false)?;
    assert_stacking(&conn, root, &[a])?;
    Ok(())
}
