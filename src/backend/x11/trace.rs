use std::cell::RefCell;
use std::fmt::{self, Write as _};
use std::io::{self, Write as _};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

static ENABLED: AtomicBool = AtomicBool::new(false);
// ~113 MiB worst case (records are 384-byte payload + ~48 header): sized so a
// ~25 s continuous llvmpipe-rate animation burst (~20 records/turn at ~500
// fps) still keeps its trailing input/action records. Grows lazily — short
// sessions never touch the full allocation.
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
        let Some(buffer) = slot.as_mut() else { return };
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
        let long = "x".repeat(PAYLOAD + 32);
        record("probe", format_args!("data={long}"));
        let buffer = take();
        assert_eq!(buffer.truncated, 1);
        assert_eq!(buffer.records.len(), 1);
        assert_eq!(buffer.records[0].len, PAYLOAD);
    }
}
