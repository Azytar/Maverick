//! Standalone X11 confirmation dialog binary.
//!
//! Exists so `quit --confirm` can prompt without GTK/Qt or shelling out to
//! `zenity`. This is the only binary outside the window manager that links
//! `x11rb`; the WM never draws dialogs.
//!
//! Role: parse `--question <text>` (or `-q` / bare positional), open a centered
//! `override_redirect` window (380×130), draw the question and Yes/No buttons
//! with core `image_text8` (Latin-1, 255-byte cap), and wait for input. Exit
//! codes: 0 confirmed (Yes/Enter/y), 1 declined (No/Esc/n/close), 2
//! usage/X11 error. Controls are click on button bounds or keycodes 36
//! (Return→yes), 9 (Escape→no), 29 (y→yes), 57 (n→no).
//!
//! Boundary: owns its X connection, window, and GC; does not own or touch WM
//! state, compositor, or control socket. Keyboard is grabbed so Enter/Esc work
//! without focus.
//!
//! # Ownership
//!
//! `run` owns the connection, window, and GC for the process lifetime;
//! `cleanup` ungrabs keyboard, frees GC, destroys window, and flushes. Buttons
//! are stack values hit-tested in the event loop.
//!
//! # Invariants
//!
//! Window is `override_redirect` and centered from `screen.width_in_pixels`.
//! Text is converted via `to_latin1`, replacing non-Latin-1 with `?` and
//! truncating at 255 bytes.

use std::process::ExitCode;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::COPY_DEPTH_FROM_PARENT;

const WIN_W: u16 = 380;
const WIN_H: u16 = 130;
const BG: u32 = 0x1e1e2e;
const FG: u32 = 0xcdd6f4;
const BTN_BG: u32 = 0x313244;
const BTN_YES: u32 = 0xa6e3a1;
const BTN_NO: u32 = 0xf38ba8;

struct Btn {
    x: i16,
    y: i16,
    w: u16,
    h: u16,
    label: &'static str,
    color: u32,
    yes: bool,
}

impl Btn {
    fn hit(&self, px: i16, py: i16) -> bool {
        px >= self.x && px < self.x + self.w as i16 && py >= self.y && py < self.y + self.h as i16
    }
}

fn main() -> ExitCode {
    let question = match parse_args() {
        Some(q) => q,
        None => {
            eprintln!("usage: maverick-dialog --question <text>");
            return ExitCode::from(2);
        }
    };

    match run(&question) {
        Ok(true) => ExitCode::SUCCESS,  // confirmed
        Ok(false) => ExitCode::FAILURE, // declined
        Err(e) => {
            eprintln!("maverick-dialog: {e}");
            ExitCode::from(2)
        }
    }
}

fn parse_args() -> Option<String> {
    let mut it = std::env::args().skip(1);
    let mut question = None;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--question" | "-q" => question = it.next(),
            other if question.is_none() => question = Some(other.to_string()),
            _ => {}
        }
    }
    question.filter(|q| !q.is_empty())
}

fn run(question: &str) -> Result<bool, Box<dyn std::error::Error>> {
    let (conn, screen_num) = x11rb::connect(None)?;
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;

    // Center the dialog on the screen.
    let x = ((screen.width_in_pixels as i32 - WIN_W as i32) / 2).max(0) as i16;
    let y = ((screen.height_in_pixels as i32 - WIN_H as i32) / 2).max(0) as i16;

    let win = conn.generate_id()?;
    conn.create_window(
        COPY_DEPTH_FROM_PARENT,
        win,
        root,
        x,
        y,
        WIN_W,
        WIN_H,
        1,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new()
            .background_pixel(BG)
            .border_pixel(FG)
            .override_redirect(1u32)
            .event_mask(
                EventMask::EXPOSURE
                    | EventMask::BUTTON_PRESS
                    | EventMask::KEY_PRESS
                    | EventMask::STRUCTURE_NOTIFY,
            ),
    )?;

    // Title (informational; override_redirect hides it from most WMs but set
    // it anyway for pagers/tools).
    conn.change_property8(
        PropMode::REPLACE,
        win,
        AtomEnum::WM_NAME,
        AtomEnum::STRING,
        b"Maverick",
    )?;

    let gc = conn.generate_id()?;
    conn.create_gc(gc, win, &CreateGCAux::new().foreground(FG).background(BG))?;

    conn.map_window(win)?;
    // Grab the keyboard so Enter/Esc work even without a WM giving us focus.
    // Both the request AND the reply status matter: an `AlreadyGrabbed`
    // success-less grab would leave the dialog deaf while holding a mapped
    // window. On any failure destroy the window before returning (a mapped,
    // unresponsive dialog is worse than none).
    let grabbed = conn
        .grab_keyboard(
            true,
            win,
            x11rb::CURRENT_TIME,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        )?
        .reply()
        .map(|r| u8::from(r.status) == 0)
        .unwrap_or(false);
    if !grabbed {
        let _ = conn.destroy_window(win);
        let _ = conn.flush();
        return Err("grab keyboard failed (already grabbed?)".into());
    }
    conn.flush()?;

    let buttons = [
        Btn {
            x: (WIN_W as i16) - 200,
            y: (WIN_H as i16) - 44,
            w: 84,
            h: 30,
            label: "Yes",
            color: BTN_YES,
            yes: true,
        },
        Btn {
            x: (WIN_W as i16) - 100,
            y: (WIN_H as i16) - 44,
            w: 84,
            h: 30,
            label: "No",
            color: BTN_NO,
            yes: false,
        },
    ];

    // The loop only exits via `break Err` (I/O, disconnect, draw failure);
    // success paths `return` above after their own cleanup. Run cleanup here
    // so the error path releases the keyboard grab too.
    let result: Result<bool, Box<dyn std::error::Error>> = loop {
        let ev = match conn.wait_for_event() {
            Ok(ev) => ev,
            // I/O error or disconnect: fall through to cleanup below instead
            // of `?`-returning with the keyboard grab still held (which would
            // freeze all keyboard input until the server resets).
            Err(e) => break Err(e.into()),
        };
        match ev {
            Event::Expose(_) => {
                // Same rule: a draw failure must still release the grab.
                if let Err(e) = draw(&conn, win, gc, question, &buttons) {
                    break Err(e);
                }
                if let Err(e) = conn.flush() {
                    break Err(e.into());
                }
            }
            Event::ButtonPress(e) => {
                for b in &buttons {
                    if b.hit(e.event_x, e.event_y) {
                        cleanup(&conn, win, gc);
                        return Ok(b.yes);
                    }
                }
            }
            Event::KeyPress(e) => {
                // Keycodes are keymap-dependent; use the common US-layout values
                // for Enter/Esc plus letters y/n. Enter=36, Esc=9 on X.Org.
                match e.detail {
                    36 => {
                        cleanup(&conn, win, gc);
                        return Ok(true);
                    } // Return
                    9 => {
                        cleanup(&conn, win, gc);
                        return Ok(false);
                    } // Escape
                    29 => {
                        cleanup(&conn, win, gc);
                        return Ok(true);
                    } // 'y'
                    57 => {
                        cleanup(&conn, win, gc);
                        return Ok(false);
                    } // 'n'
                    _ => {}
                }
            }
            _ => {}
        }
    };
    cleanup(&conn, win, gc);
    result
}

fn draw(
    conn: &impl Connection,
    win: Window,
    gc: u32,
    question: &str,
    buttons: &[Btn],
) -> Result<(), Box<dyn std::error::Error>> {
    // Clear background.
    conn.change_gc(gc, &ChangeGCAux::new().foreground(BG))?;
    conn.poly_fill_rectangle(
        win,
        gc,
        &[Rectangle {
            x: 0,
            y: 0,
            width: WIN_W,
            height: WIN_H,
        }],
    )?;

    // Question text (Latin-1; image_text8 caps at 255 bytes).
    conn.change_gc(gc, &ChangeGCAux::new().foreground(FG).background(BG))?;
    let text = to_latin1(question);
    conn.image_text8(win, gc, 20, 40, &text)?;

    // Buttons.
    for b in buttons {
        conn.change_gc(gc, &ChangeGCAux::new().foreground(BTN_BG))?;
        conn.poly_fill_rectangle(
            win,
            gc,
            &[Rectangle {
                x: b.x,
                y: b.y,
                width: b.w,
                height: b.h,
            }],
        )?;
        conn.change_gc(
            gc,
            &ChangeGCAux::new().foreground(b.color).background(BTN_BG),
        )?;
        let label = to_latin1(b.label);
        let tx = b.x + (b.w as i16 - (b.label.len() as i16 * 6)) / 2;
        conn.image_text8(win, gc, tx.max(b.x + 6), b.y + 20, &label)?;
    }

    Ok(())
}

fn cleanup(conn: &impl Connection, win: Window, gc: u32) {
    let _ = conn.ungrab_keyboard(x11rb::CURRENT_TIME);
    let _ = conn.free_gc(gc);
    let _ = conn.destroy_window(win);
    let _ = conn.flush();
}

/// UTF-8 → Latin-1 for the default X core font (image_text8 is 8-bit).
fn to_latin1(s: &str) -> Vec<u8> {
    s.chars()
        .map(|c| if (c as u32) <= 0xff { c as u8 } else { b'?' })
        .take(255)
        .collect()
}
