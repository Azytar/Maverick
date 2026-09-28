//! Live-driver tests: everything here needs a Vulkan driver, a display and a
//! real window, so every test is `#[ignore]`d and skips itself unless
//! `MAVERICK_VK_SMOKE=1` is set.
//!
//! They are `#[ignore]`d rather than absent because the object-lifetime
//! questions they answer — is the teardown legal with a frame in flight, does the
//! surface survive its X connection — can only be answered by a driver, and by
//! the Khronos validation layer rather than by any host-side assertion. A CI box
//! without a GPU gets a green suite and these are exactly the checks it cannot
//! run.
//!
//! To run them on a machine that has a driver:
//!
//! ```sh
//! cargo test -p maverick-vk --test smoke -- --ignored --nocapture --test-threads=1
//! MAVERICK_VK_SMOKE=1 MAVERICK_VK_VALIDATION=1 \
//!     cargo test -p maverick-vk --test smoke -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Without `MAVERICK_VK_SMOKE=1` each test prints the reason it skipped and
//! passes. `--test-threads=1` matters for the validation check, which re-executes
//! this same test binary as a child and reads the child's stderr: a concurrent
//! test would interleave its output into that stream.

use maverick_vk::{SurfaceTarget, Vulkan};
use maverick_x11::open_x;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{self, ConnectionExt};

const WIDTH: u16 = 320;
const HEIGHT: u16 = 240;

/// Marks the re-executed child process of `drop_after_present_is_validation_clean`.
const CHILD: &str = "MAVERICK_VK_DROP_CHILD";

#[test]
#[ignore]
fn smoke_init_and_present() {
    if std::env::var("MAVERICK_VK_SMOKE").as_deref() != Ok("1") {
        eprintln!("smoke: skipped (MAVERICK_VK_SMOKE != 1)");
        return;
    }
    if std::env::var("DISPLAY").is_err() {
        eprintln!("smoke: skipped (DISPLAY unset)");
        return;
    }

    let (_display, conn, screen_num) = open_x().expect("open_x");
    let xcb_connection = conn.get_raw_xcb_connection();

    let window = spawn_test_window(&conn, screen_num);
    // Best-effort cleanup of the test window in every exit path.
    let cleanup = || {
        let _ = conn.destroy_window(window);
        let _ = conn.flush();
    };

    let target = SurfaceTarget {
        xcb_connection,
        window,
        width: WIDTH as u32,
        height: HEIGHT as u32,
    };

    let mut vk = match Vulkan::new(target) {
        Ok(v) => v,
        Err(e) => {
            cleanup();
            panic!("Vulkan::new failed: {e}");
        }
    };
    println!(
        "smoke: {}\nformat={:?} extent={:?}",
        vk.report(),
        vk.format(),
        vk.extent()
    );

    let frames = 3;
    for i in 0..frames {
        let t = i as f32 / frames as f32;
        if let Err(e) = vk.acquire_and_present([t, 0.2, 1.0 - t, 1.0]) {
            cleanup();
            panic!("present frame {i} failed: {e}");
        }
    }

    cleanup();
    // `_display` and `conn` are still live here and are dropped after `vk`,
    // whose surface borrows their `xcb_connection_t*`; the kernel reaps the
    // socket at process exit.
}

/// `acquire_and_present` returns as soon as the frame is submitted, so the
/// objects `Drop` destroys may still be executing. Whether the destruction is
/// legal is a Vulkan object-lifetime question no host-side assertion can
/// answer, and only the Khronos validation layer reports it — through the
/// messenger this crate installs, which writes to stderr. Hence the child
/// process: the frame runs there and this test reads back its stderr.
#[test]
#[ignore]
fn drop_after_present_is_validation_clean() {
    // Child half: do the work the parent is going to inspect.
    if std::env::var(CHILD).as_deref() == Ok("1") {
        frame_then_drop();
        return;
    }

    if std::env::var("MAVERICK_VK_SMOKE").as_deref() != Ok("1") {
        eprintln!("drop-check: skipped (MAVERICK_VK_SMOKE != 1)");
        return;
    }
    if std::env::var("DISPLAY").is_err() {
        eprintln!("drop-check: skipped (DISPLAY unset)");
        return;
    }
    if !validation_layer_available() {
        eprintln!("drop-check: skipped (VK_LAYER_KHRONOS_validation not installed)");
        return;
    }

    let exe = std::env::current_exe().expect("current_exe");
    let out = std::process::Command::new(exe)
        .args([
            "--exact",
            "drop_after_present_is_validation_clean",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .env("MAVERICK_VK_VALIDATION", "1")
        .output()
        .expect("re-exec self");

    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "child run failed:\n{stderr}");

    let errors: Vec<&str> = stderr
        .lines()
        .filter(|l| l.contains("[vulkan-validation ERROR]"))
        .collect();
    assert!(
        errors.is_empty(),
        "tearing the backend down left submitted work in flight:\n{}\n\
         full child stderr:\n{stderr}",
        errors.join("\n"),
    );
}

/// Present a single frame, then let `Vulkan` go out of scope with no
/// synchronization from the caller — the ordinary way a WM shuts a backend
/// down. One frame only: a second one would re-use the presentation semaphore
/// and report that separately.
fn frame_then_drop() {
    let (_display, conn, screen_num) = open_x().expect("open_x");
    let xcb_connection = conn.get_raw_xcb_connection();
    let window = spawn_test_window(&conn, screen_num);

    let target = SurfaceTarget {
        xcb_connection,
        window,
        width: WIDTH as u32,
        height: HEIGHT as u32,
    };

    let mut vk = Vulkan::new(target).expect("Vulkan::new");
    vk.acquire_and_present([0.0, 0.2, 1.0, 1.0])
        .expect("acquire_and_present");
    // No explicit wait: the throwaway window is reaped by the X server when
    // this process exits, moments after `vk` drops here.
}

/// Create and map the window the Vulkan surface is anchored to. Its own
/// override-redirect window: a managed window would be picked up by the
/// running compositor's overlay and damage tracking.
fn spawn_test_window<C: Connection>(conn: &C, screen_num: usize) -> u32 {
    let setup = conn.setup();
    let screen = &setup.roots[screen_num];
    let window = conn.generate_id().unwrap();
    let aux = xproto::CreateWindowAux::new().override_redirect(Some(1u32));
    conn.create_window(
        screen.root_depth,
        window,
        screen.root,
        0,
        0,
        WIDTH,
        HEIGHT,
        0,
        xproto::WindowClass::INPUT_OUTPUT,
        screen.root_visual,
        &aux,
    )
    .expect("create_window");
    conn.map_window(window).expect("map_window");
    conn.flush().expect("flush");
    window
}

/// Whether the check can observe anything at all: without the layer installed
/// no validation error is ever printed and the assertion would pass vacuously.
fn validation_layer_available() -> bool {
    // SAFETY: `load` only dlopens the Vulkan loader; no Vulkan object is involved
    // and the only thing it needs from the host is a loader Maverick can speak.
    let Ok(entry) = (unsafe { ash::Entry::load() }) else {
        return false;
    };
    // SAFETY: `entry` is the live loader just loaded, and the layer list is
    // driver-reported data about layers rather than an object, so there is no
    // handle validity or lifetime question left to answer.
    let Ok(props) = (unsafe { entry.enumerate_instance_layer_properties() }) else {
        return false;
    };
    props.iter().any(|p| {
        // `layer_name_as_c_str` is `ash`'s bounded reader, so a layer that
        // failed to NUL-terminate its own name yields `Err` rather than a read
        // past the end of the array.
        p.layer_name_as_c_str() == Ok(c"VK_LAYER_KHRONOS_validation")
    })
}
