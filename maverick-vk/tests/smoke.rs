// maverick-vk/tests/smoke.rs
//
// Optional, env-gated smoke test. It is `#[ignore]`d so `cargo test --workspace`
// stays green on machines with no GPU, and additionally requires
// `MAVERICK_VK_SMOKE=1` *and* `DISPLAY`. It reuses `maverick_gl::open_x()` to
// get the shared XCB connection and creates its OWN override-redirect window so
// the live compositor's overlay is never disturbed; the test window is destroyed
// before returning.

use maverick_gl::open_x;
use maverick_vk::{SurfaceTarget, Vulkan};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{self, ConnectionExt};

const WIDTH: u16 = 320;
const HEIGHT: u16 = 240;

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
    // `_display` / `conn` are intentionally left alive: the kernel closes the
    // socket at process exit, and nothing here calls Xlib event functions.
}
