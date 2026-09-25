//! Fixed-size in-memory trace of one session, dumped as TSV on shutdown.
//!
//! Enabled at runtime with `MAVERICK_COMPOSITOR_TRACE=1` (path overridable with
//! `MAVERICK_COMPOSITOR_TRACE_PATH`, default `$TMPDIR/maverick-compositor-<pid>.trace`),
//! not by a cargo feature. This is a distinct mechanism from the compile-time
//! `input-trace` / `window-trace` stderr macros in `render.rs`, `pointer.rs` and
//! `reconciler.rs`: those cost nothing when the feature is off, while this ring
//! buffer is always compiled in and only touches memory when the env var is set.
//!
//! The buffer is thread-local and owned by the WM thread, so `record` needs no
//! locking on the event-loop hot path; a secondary thread simply gets its own
//! empty buffer. `init()` installs it before the X connection is opened,
//! `dump()` writes and releases it during `cleanup()`.

use std::cell::RefCell;
use std::fmt::{self, Write as _};
use std::io::{self, Write as _};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

static ENABLED: AtomicBool = AtomicBool::new(false);
// ~112 MiB resident at the cap: 448 bytes per record (384-byte payload + a
// 64-byte header), so a continuous software-GL animation burst still keeps its
// trailing input/action records. The `Vec` starts at 4096 entries and doubles,
// so a short session never pays for the full cap.
const CAPACITY: usize = 262_144;
const PAYLOAD: usize = 384;

struct Record {
    ns: u128,
    turn: u64,
    frame: u64,
    event: &'static str,
    bytes: [u8; PAYLOAD],
    len: usize,
}

impl fmt::Write for Record {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        // Fixed inline payload: no allocation on the hot path, and an
        // overflowing write is truncated at a char boundary (never mid-UTF-8)
        // and reported as an error so the caller can count it.
        let available = PAYLOAD - self.len;
        let mut count = text.len().min(available);
        while !text.is_char_boundary(count) {
            count -= 1;
        }
        self.bytes[self.len..self.len + count].copy_from_slice(&text.as_bytes()[..count]);
        self.len += count;
        if count == text.len() {
            Ok(())
        } else {
            Err(fmt::Error)
        }
    }
}

struct Buffer {
    start: Instant,
    path: PathBuf,
    records: Vec<Record>,
    turn: u64,
    frame: u64,
    dropped: u64,
    truncated: u64,
    last_frame: Option<Instant>,
    capacity: usize,
}

thread_local! {
    static BUFFER: RefCell<Option<Buffer>> = const { RefCell::new(None) };
}

pub(super) fn init() {
    if std::env::var_os("MAVERICK_COMPOSITOR_TRACE").as_deref() != Some(std::ffi::OsStr::new("1")) {
        return;
    }
    let path = std::env::var_os("MAVERICK_COMPOSITOR_TRACE_PATH").map_or_else(
        || std::env::temp_dir().join(format!("maverick-compositor-{}.trace", std::process::id())),
        PathBuf::from,
    );
    BUFFER.with(|slot| {
        *slot.borrow_mut() = Some(Buffer {
            start: Instant::now(),
            path,
            records: Vec::with_capacity(4096),
            turn: 0,
            frame: 0,
            dropped: 0,
            truncated: 0,
            last_frame: None,
            capacity: CAPACITY,
        });
    });
    ENABLED.store(true, Ordering::Relaxed);
    // Anchor the trace's `Instant` clock against CLOCK_MONOTONIC, so a dump can
    // be lined up with an X11 client-side log without trusting wall time.
    let mut timestamp = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    record("clock_anchor_before", format_args!(""));
    let valid = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut timestamp) } == 0;
    record(
        "clock_anchor_after",
        format_args!(
            "valid={valid} monotonic_ns={}",
            i128::from(timestamp.tv_sec) * 1_000_000_000 + i128::from(timestamp.tv_nsec)
        ),
    );
}

#[inline]
pub(super) fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

pub(super) fn record(event: &'static str, args: fmt::Arguments<'_>) {
    BUFFER.with(|slot| {
        let mut slot = slot.borrow_mut();
        // A thread that never ran `init` (the control-socket thread) has no
        // buffer and records nothing.
        let Some(buffer) = slot.as_mut() else { return };
        // Past the cap, drop the record instead of overwriting: the tail is what
        // explains the symptom the trace was collected for.
        if buffer.records.len() == buffer.capacity {
            buffer.dropped += 1;
            return;
        }
        let mut record = Record {
            ns: buffer.start.elapsed().as_nanos(),
            turn: buffer.turn,
            frame: buffer.frame,
            event,
            bytes: [0; PAYLOAD],
            len: 0,
        };
        if record.write_fmt(args).is_err() {
            buffer.truncated += 1;
        }
        buffer.records.push(record);
    });
}

pub(super) fn begin_turn() {
    if enabled() {
        BUFFER.with(|slot| {
            if let Some(buffer) = slot.borrow_mut().as_mut() {
                buffer.turn += 1;
            }
        });
        record("turn_begin", format_args!(""));
    }
}

pub(super) fn begin_frame() {
    if enabled() {
        // The interval is measured between *begins*, not ends: it shows the GL
        // attempt rate the scheduler actually asked for, which is the signal
        // that distinguishes "no frame requested" from "frame requested too
        // slowly". `None` on the first frame.
        let interval = BUFFER.with(|slot| {
            let mut slot = slot.borrow_mut();
            let buffer = slot.as_mut()?;
            let now = Instant::now();
            buffer.frame += 1;
            let interval = buffer
                .last_frame
                .map(|last| now.duration_since(last).as_nanos());
            buffer.last_frame = Some(now);
            interval
        });
        record(
            "frame_begin",
            format_args!("previous_begin_interval_ns={interval:?}"),
        );
    }
}

/// Write the buffer out and disable tracing for the rest of the process. The
/// header line is the format contract: it pins the clock, the units and the two
/// caveats every reader has to keep in mind — a `swap` that returned is not
/// proof the frame was presented, and a window whose geometry was set to 0 for
/// an off-screen tile is not evidence of a GL present.
pub(super) fn dump() {
    if !ENABLED.swap(false, Ordering::Relaxed) {
        return;
    }
    let buffer = BUFFER.with(|slot| slot.borrow_mut().take());
    if let Some(buffer) = buffer {
        let result = (|| -> io::Result<()> {
            let mut out = io::BufWriter::new(std::fs::File::create(&buffer.path)?);
            writeln!(
                out,
                "# maverick_compositor_trace_v1 clock=Instant units=ns x_time=server_ms capacity={} dropped={} truncated={} swap_returned_is_not_visible=true off_geometry_is_not_gl_present=true",
                buffer.capacity, buffer.dropped, buffer.truncated
            )?;
            writeln!(
                out,
                "# ns\tturn\tframe\tevent\tfields (frame is latest begun GL attempt; startup=0)"
            )?;
            for record in buffer.records {
                write!(
                    out,
                    "{}\t{}\t{}\t{}\t",
                    record.ns, record.turn, record.frame, record.event
                )?;
                out.write_all(&record.bytes[..record.len])?;
                writeln!(out)?;
            }
            out.flush()
        })();
        if let Err(error) = result {
            eprintln!("compositor trace dump {}: {error}", buffer.path.display());
        }
    }
}

/// Record the X11 event that triggered this turn. `x_time_ms` is the server
/// timestamp, which is *not* comparable with the trace's `ns` column: mixing the
/// two is what makes an input/frame correlation look impossible when it is only
/// skewed by clock domains.
pub(super) fn input(event: &x11rb::protocol::Event) {
    use x11rb::protocol::Event;
    let (kind, time, window, detail, state) = match event {
        Event::KeyPress(e) => ("key_press", e.time, e.event, e.detail, u16::from(e.state)),
        Event::KeyRelease(e) => ("key_release", e.time, e.event, e.detail, u16::from(e.state)),
        Event::ButtonPress(e) => (
            "button_press",
            e.time,
            e.event,
            e.detail,
            u16::from(e.state),
        ),
        Event::ButtonRelease(e) => (
            "button_release",
            e.time,
            e.event,
            e.detail,
            u16::from(e.state),
        ),
        Event::MotionNotify(e) => ("motion", e.time, e.event, 0, u16::from(e.state)),
        Event::EnterNotify(e) => ("enter", e.time, e.event, 0, u16::from(e.state)),
        Event::LeaveNotify(e) => ("leave", e.time, e.event, 0, u16::from(e.state)),
        _ => {
            record(
                "event_receipt",
                format_args!("opcode={}", event.response_type()),
            );
            return;
        }
    };
    record(
        "input_receipt",
        format_args!("kind={kind} x_time_ms={time} win={window} detail={detail} state={state}"),
    );
}

/// RAII boundary marker: emits `boundary=begin` on construction and
/// `boundary=end duration_ns=…` on drop, so an early return inside a traced
/// block still closes its span. Holds the start time only when tracing is on.
pub(super) struct Span {
    event: &'static str,
    start: Option<Instant>,
}

impl Span {
    pub(super) fn new(event: &'static str) -> Self {
        let start = if enabled() {
            record(event, format_args!("boundary=begin"));
            Some(Instant::now())
        } else {
            None
        };
        Self { event, start }
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        if let Some(start) = self.start {
            let ns = start.elapsed().as_nanos();
            record(self.event, format_args!("boundary=end duration_ns={ns}"));
        }
    }
}

/// One-line trace at an arbitrary site, gated on `enabled()` so the argument
/// formatting costs nothing when the trace is off.
macro_rules! trace {
    ($event:expr, $($args:tt)*) => {
        if $crate::backend::x11::trace::enabled() {
            $crate::backend::x11::trace::record($event, format_args!($($args)*));
        }
    };
}
pub(super) use trace;

#[cfg(test)]
mod tests {
    use super::*;

    fn init_buffer_with_capacity(capacity: usize) {
        BUFFER.with(|slot| {
            *slot.borrow_mut() = Some(Buffer {
                start: Instant::now(),
                path: std::env::temp_dir().join("maverick-trace-test"),
                records: Vec::with_capacity(capacity),
                turn: 0,
                frame: 0,
                dropped: 0,
                truncated: 0,
                last_frame: None,
                capacity,
            });
        });
        ENABLED.store(true, Ordering::Relaxed);
    }

    fn take() -> Buffer {
        ENABLED.store(false, Ordering::Relaxed);
        BUFFER
            .with(|slot| slot.borrow_mut().take())
            .expect("buffer must be initialised")
    }

    #[test]
    fn records_bounded_and_counted_when_full() {
        init_buffer_with_capacity(4);
        // Past the cap the *earliest* records survive: a trace whose head is
        // overwritten cannot explain why the burst started.
        for i in 0..7 {
            record("probe", format_args!("i={i}"));
        }
        let buffer = take();
        assert_eq!(buffer.records.len(), 4);
        assert_eq!(buffer.dropped, 3);
        assert_eq!(buffer.records[0].event, "probe");
    }

    #[test]
    fn frames_and_turns_are_monotonic_with_intervals() {
        init_buffer_with_capacity(16);
        begin_turn();
        begin_frame();
        begin_frame();
        begin_turn();
        begin_frame();
        let buffer = take();
        assert_eq!(buffer.turn, 2);
        assert_eq!(buffer.frame, 3);
        let frame_count = buffer
            .records
            .iter()
            .filter(|r| r.event == "frame_begin")
            .count();
        assert_eq!(frame_count, 3);
        for window in buffer.records.windows(2) {
            assert!(
                window[0].ns <= window[1].ns,
                "records must stay in arrival order"
            );
        }
    }

    #[test]
    fn payload_overflow_truncates_and_counts() {
        init_buffer_with_capacity(1);
        // An over-long payload is truncated, never dropped: losing the record
        // would hide the event that produced it.
        let long = "x".repeat(PAYLOAD + 32);
        record("probe", format_args!("data={long}"));
        let buffer = take();
        assert_eq!(buffer.truncated, 1);
        assert_eq!(buffer.records.len(), 1);
        assert_eq!(buffer.records[0].len, PAYLOAD);
    }
}
