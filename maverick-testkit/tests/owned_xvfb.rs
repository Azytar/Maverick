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
    Atom, AtomEnum, ConnectionExt, CreateWindowAux, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;

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
    let values = reply.value32().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "invalid X11 window property")
    })?;
    Ok(values.collect())
}

fn release_binary() -> PathBuf {
    std::env::var_os("MAVERICK_TEST_WM")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/release/maverick"))
}

#[test]
#[ignore = "needs Xvfb and an already compiled release WM"]
fn managed_windows_follow_real_server_stacking() -> Result<(), Box<dyn Error>> {
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
    let client_list = atom(&conn, b"_NET_CLIENT_LIST")?;
    let stacking = atom(&conn, b"_NET_CLIENT_LIST_STACKING")?;

    loop {
        if !window_list(&conn, root, supporting_wm)?.is_empty() {
            break;
        }
        if let Some(status) = session.wm_exit_status()? {
            return Err(format!("Maverick exited before readiness: {status}").into());
        }
        if Instant::now() >= deadline {
            return Err("Maverick did not announce EWMH readiness".into());
        }
        thread::sleep(Duration::from_millis(25));
    }

    let windows = [conn.generate_id()?, conn.generate_id()?];
    for (index, window) in windows.into_iter().enumerate() {
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            root,
            20 + (index as i16) * 30,
            20,
            180,
            120,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::default(),
        )?
        .check()?;
        conn.map_window(window)?.check()?;
    }
    conn.flush()?;

    loop {
        let tracked = window_list(&conn, root, client_list)?;
        if windows.iter().all(|window| tracked.contains(window)) {
            break;
        }
        if let Some(status) = session.wm_exit_status()? {
            return Err(format!("Maverick exited while managing windows: {status}").into());
        }
        if Instant::now() >= deadline {
            return Err("Maverick did not manage both X11 clients".into());
        }
        thread::sleep(Duration::from_millis(25));
    }

    let published_order = loop {
        let published: Vec<_> = window_list(&conn, root, stacking)?
            .into_iter()
            .filter(|window| windows.contains(window))
            .collect();
        if published.len() == windows.len() {
            break published;
        }
        if Instant::now() >= deadline {
            return Err("Maverick did not publish both managed clients in stacking".into());
        }
        thread::sleep(Duration::from_millis(25));
    };
    let server_order: Vec<_> = conn
        .query_tree(root)?
        .reply()?
        .children
        .into_iter()
        .filter(|window| windows.contains(window))
        .collect();
    assert_eq!(server_order.len(), 2, "both managed clients must appear in QueryTree");
    assert_eq!(server_order, published_order, "EWMH stacking must match the real X server");
    Ok(())
}
