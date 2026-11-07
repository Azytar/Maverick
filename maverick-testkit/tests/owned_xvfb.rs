//! A Rust X11 stacking oracle, run against the release WM on a private Xvfb.
//! The existing Python regression remains in CI until equivalence is proven.

use std::error::Error;
use std::io;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use maverick_testkit::XvfbSession;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageEvent, ConnectionExt, CreateWindowAux, EventMask, PropMode,
    Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as WrapperConnectionExt;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(12);

fn atom(conn: &RustConnection, name: &[u8]) -> Result<Atom, Box<dyn Error>> {
    Ok(conn.intern_atom(false, name)?.reply()?.atom)
}

fn window_list(
    conn: &RustConnection,
    root: Window,
    property: Atom,
) -> Result<Vec<Window>, Box<dyn Error>> {
    let reply = conn
        .get_property(false, root, property, AtomEnum::WINDOW, 0, 1024)?
        .reply()?;
    if reply.format == 0 {
        // The WM may not have published this property yet.
        return Ok(Vec::new());
    }
    let values = reply
        .value32()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid X11 window property"))?;
    Ok(values.collect())
}

fn release_binary() -> PathBuf {
    std::env::var_os("MAVERICK_TEST_WM")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/release/maverick")
        })
}

fn wait_for(
    session: &mut XvfbSession,
    description: &str,
    mut condition: impl FnMut() -> Result<bool, Box<dyn Error>>,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        if condition()? {
            return Ok(());
        }
        if let Some(status) = session.wm_exit_status()? {
            return Err(format!("Maverick exited during {description}: {status}").into());
        }
        if Instant::now() >= deadline {
            return Err(format!("timed out waiting for {description}").into());
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn start_session() -> Result<(XvfbSession, RustConnection, Window), Box<dyn Error>> {
    let binary = release_binary();
    if !binary.is_file() {
        return Err(format!("missing release WM: {}", binary.display()).into());
    }

    let mut session = XvfbSession::start()?;
    session.launch_wm(&binary)?;

    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let (conn, screen) = loop {
        match RustConnection::connect(Some(session.display())) {
            Ok(conn) => break conn,
            Err(_) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(error.into()),
        }
    };
    let root = conn.setup().roots[screen].root;
    let supporting_wm = atom(&conn, b"_NET_SUPPORTING_WM_CHECK")?;
    wait_for(&mut session, "EWMH readiness", || {
        Ok(!window_list(&conn, root, supporting_wm)?.is_empty())
    })?;
    Ok((session, conn, root))
}

fn map_client(
    conn: &RustConnection,
    root: Window,
    floating: bool,
) -> Result<Window, Box<dyn Error>> {
    let window = conn.generate_id()?;
    conn.create_window(
        x11rb::COPY_DEPTH_FROM_PARENT,
        window,
        root,
        20,
        20,
        180,
        120,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::default(),
    )?
    .check()?;
    if floating {
        // EWMH dialog type makes this a genuine float without control IPC.
        let kind = atom(conn, b"_NET_WM_WINDOW_TYPE")?;
        let dialog = atom(conn, b"_NET_WM_WINDOW_TYPE_DIALOG")?;
        conn.change_property32(PropMode::REPLACE, window, kind, AtomEnum::ATOM, &[dialog])?
            .check()?;
    }
    conn.map_window(window)?.check()?;
    conn.flush()?;
    Ok(window)
}

fn assert_stacking(
    conn: &RustConnection,
    root: Window,
    windows: &[Window],
) -> Result<Vec<Window>, Box<dyn Error>> {
    let stacking = atom(conn, b"_NET_CLIENT_LIST_STACKING")?;
    let server_order: Vec<_> = conn
        .query_tree(root)?
        .reply()?
        .children
        .into_iter()
        .filter(|window| windows.contains(window))
        .collect();
    let published_order: Vec<_> = window_list(conn, root, stacking)?
        .into_iter()
        .filter(|window| windows.contains(window))
        .collect();
    assert_eq!(
        server_order.len(),
        windows.len(),
        "QueryTree must contain every managed client"
    );
    assert_eq!(
        server_order, published_order,
        "EWMH stacking must match the real X server"
    );
    Ok(server_order)
}

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
        let event = ClientMessageEvent::new(32, target, active, [2, 0, 0, 0, 0]);
        conn.send_event(
            false,
            root,
            EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
            event,
        )?
        .check()?;
        conn.flush()?;

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
