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
use x11rb::rust_connection::{DefaultStream, PollMode, Stream};
use x11rb::utils::RawFdContainer;

/// The watchdog must drop with this X11 connection, never with the session:
/// keeping its duplicate socket alive would prevent server-side client death.
pub struct DeadlineStream {
    inner: DefaultStream,
    _deadline: maverick_testkit::ConnectionDeadline,
}

impl Stream for DeadlineStream {
    fn poll(&self, mode: PollMode) -> io::Result<()> {
        self.inner.poll(mode)
    }
    fn read(&self, bytes: &mut [u8], fds: &mut Vec<RawFdContainer>) -> io::Result<usize> {
        self.inner.read(bytes, fds)
    }
    fn write(&self, bytes: &[u8], fds: &mut Vec<RawFdContainer>) -> io::Result<usize> {
        self.inner.write(bytes, fds)
    }
}

pub type RustConnection = x11rb::rust_connection::RustConnection<DeadlineStream>;

pub fn connect(session: &XvfbSession) -> Result<RustConnection, Box<dyn Error>> {
    let (stream, deadline) = session.connect_stream()?;
    let (inner, _) = DefaultStream::from_unix_stream(stream)?;
    Ok(RustConnection::connect_to_stream(
        DeadlineStream {
            inner,
            _deadline: deadline,
        },
        0,
    )?)
}
use x11rb::wrapper::ConnectionExt as WrapperConnectionExt;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(12);

pub fn atom(conn: &RustConnection, name: &[u8]) -> Result<Atom, Box<dyn Error>> {
    Ok(conn.intern_atom(false, name)?.reply()?.atom)
}

pub fn window_list(
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
    if reply.type_ != u32::from(AtomEnum::WINDOW) || reply.format != 32 || reply.bytes_after != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid or truncated EWMH window list",
        )
        .into());
    }
    let values = reply
        .value32()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid X11 window property"))?;
    Ok(values.collect())
}

pub fn release_binary() -> PathBuf {
    std::env::var_os("MAVERICK_TEST_WM")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/release/maverick")
        })
}

pub fn wait_for(
    session: &mut XvfbSession,
    description: &str,
    mut condition: impl FnMut() -> Result<bool, Box<dyn Error>>,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        if let Some(status) = session.server_exit_status()? {
            return Err(format!(
                "Xvfb exited during {description}: {status}\n{}",
                session.diagnostics()
            )
            .into());
        }
        if let Some(status) = session.wm_exit_status()? {
            return Err(format!(
                "Maverick exited during {description}: {status}\n{}",
                session.diagnostics()
            )
            .into());
        }
        if condition().map_err(|error| -> Box<dyn Error> {
            format!("{description}: {error}\n{}", session.diagnostics()).into()
        })? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for {description}\n{}",
                session.diagnostics()
            )
            .into());
        }
        thread::sleep(Duration::from_millis(25));
    }
}

pub fn start_session() -> Result<(XvfbSession, RustConnection, Window), Box<dyn Error>> {
    start_with_config(
        "[general]\nfocus_mouse = false\nwarp_cursor = false\n[autostart]\ncommands = []\n",
    )
}

pub fn start_with_config(
    config: &str,
) -> Result<(XvfbSession, RustConnection, Window), Box<dyn Error>> {
    let binary = release_binary();
    if !binary.is_file() {
        return Err(format!("missing release WM: {}", binary.display()).into());
    }

    let mut session = XvfbSession::with_config(config)?;
    session.launch_wm(&binary)?;

    let conn = connect(&session)?;
    let screen = 0;
    let root = conn.setup().roots[screen].root;
    let supporting_wm = atom(&conn, b"_NET_SUPPORTING_WM_CHECK")?;
    wait_for(&mut session, "EWMH readiness", || {
        Ok(!window_list(&conn, root, supporting_wm)?.is_empty())
    })?;
    Ok((session, conn, root))
}

pub fn map_client(
    conn: &RustConnection,
    root: Window,
    floating: bool,
) -> Result<Window, Box<dyn Error>> {
    let window = create_client(conn, root, floating)?;
    conn.map_window(window)?.check()?;
    conn.flush()?;
    Ok(window)
}

pub fn create_client(
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
    Ok(window)
}

pub fn request_focus(
    conn: &RustConnection,
    root: Window,
    active: Atom,
    target: Window,
) -> Result<(), Box<dyn Error>> {
    // EWMH pager request: no control socket or CLI is involved.
    let event = ClientMessageEvent::new(32, target, active, [2, 0, 0, 0, 0]);
    conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
        event,
    )?
    .check()?;
    conn.flush()?;
    Ok(())
}

pub fn assert_stacking(
    conn: &RustConnection,
    root: Window,
    windows: &[Window],
) -> Result<Vec<Window>, Box<dyn Error>> {
    let stacking = atom(conn, b"_NET_CLIENT_LIST_STACKING")?;
    let all_published = window_list(conn, root, stacking)?;
    let mut unique = all_published.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), all_published.len(), "duplicate EWMH entries");
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

/// Read the actual server order without consulting the WM metadata.
pub fn server_stack(
    conn: &RustConnection,
    root: Window,
    windows: &[Window],
) -> Result<Vec<Window>, Box<dyn Error>> {
    Ok(conn
        .query_tree(root)?
        .reply()?
        .children
        .into_iter()
        .filter(|w| windows.contains(w))
        .collect())
}

pub fn wait_managed(
    session: &mut XvfbSession,
    conn: &RustConnection,
    root: Window,
    windows: &[Window],
) -> Result<(), Box<dyn Error>> {
    let clients = atom(conn, b"_NET_CLIENT_LIST")?;
    let stack = atom(conn, b"_NET_CLIENT_LIST_STACKING")?;
    wait_for(session, "client registration and stacking metadata", || {
        let list = window_list(conn, root, clients)?;
        let order = window_list(conn, root, stack)?;
        Ok(windows
            .iter()
            .all(|w| list.contains(w) && order.contains(w)))
    })
}

pub fn set_state(
    conn: &RustConnection,
    root: Window,
    window: Window,
    action: u32,
    name: &[u8],
) -> Result<(), Box<dyn Error>> {
    send_message(
        conn,
        root,
        window,
        b"_NET_WM_STATE",
        [action, atom(conn, name)?, 0, 1, 0],
    )
}

pub fn send_message(
    conn: &RustConnection,
    root: Window,
    window: Window,
    name: &[u8],
    data: [u32; 5],
) -> Result<(), Box<dyn Error>> {
    conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
        ClientMessageEvent::new(32, window, atom(conn, name)?, data),
    )?
    .check()?;
    conn.flush()?;
    Ok(())
}

pub fn has_state(
    conn: &RustConnection,
    window: Window,
    name: &[u8],
) -> Result<bool, Box<dyn Error>> {
    let state = atom(conn, b"_NET_WM_STATE")?;
    let expected = atom(conn, name)?;
    Ok(conn
        .get_property(false, window, state, AtomEnum::ATOM, 0, 1024)?
        .reply()?
        .value32()
        .is_some_and(|mut values| values.any(|a| a == expected)))
}

pub fn settled_geometry(
    session: &mut XvfbSession,
    conn: &RustConnection,
    window: Window,
) -> Result<x11rb::protocol::xproto::GetGeometryReply, Box<dyn Error>> {
    let mut last = conn.get_geometry(window)?.reply()?;
    let mut stable_since = Instant::now();
    wait_for(session, "settled client geometry", || {
        let next = conn.get_geometry(window)?.reply()?;
        if (next.x, next.y, next.width, next.height, next.border_width)
            != (last.x, last.y, last.width, last.height, last.border_width)
        {
            last = next;
            stable_since = Instant::now();
        }
        Ok(stable_since.elapsed() >= Duration::from_millis(200))
    })?;
    Ok(last)
}
