// maverick-dialog — a tiny, dependency-light X11 confirmation dialog.
//
// It exists so `maverickctl quit --confirm` can pop a native prompt without
// pulling GTK/Qt or shelling out to zenity. It is a *separate* binary and the
// only place in the project that links x11rb outside the WM itself — the WM
// never draws dialogs.
//
// Usage:
//   maverick-dialog --question "Quit Maverick?"
//
// Exit code:
//   0  user confirmed (Yes / Enter / y)
//   1  user declined  (No / Esc / n / window closed)
//   2  usage or X11 error
//
// Controls: click Yes/No, or press y/Enter (yes) or n/Esc (no).

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
    let _ = conn.grab_keyboard(
        true,
        win,
        x11rb::CURRENT_TIME,
        GrabMode::ASYNC,
        GrabMode::ASYNC,
    )?;
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

    loop {
        let ev = conn.wait_for_event()?;
        match ev {
            Event::Expose(_) => {
                draw(&conn, win, gc, question, &buttons)?;
                conn.flush()?;
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
    }
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
