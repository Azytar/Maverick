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
//! `dump()` writes and releases it during the shutdown, on *every* exit path —
//! including the one where the X server died, which is the path that used to
//! lose the whole buffer without a word in the log.

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

/// Serialises the tests in this file against each other.
///
/// `BUFFER` is thread-local, so each test gets its own ring — but `ENABLED` is a
/// process-global, and `begin_turn`/`begin_frame` read it to decide whether to
/// bump the counter. Two tests running concurrently can therefore interleave
/// one test's `take()` (which clears the global) into another's
/// `init`/`begin` window, and the second test silently loses a boundary. The
/// buffer helpers in `tests` predate the property tests below and did not need
/// this; adding property tests that drive `begin_turn`/`begin_frame` made the
/// window reachable, so both halves now take the same guard.
#[cfg(test)]
static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Acquire [`TEST_LOCK`], ignoring poisoning: a panicking trace test leaves no
/// inconsistent state behind, and refusing to run the others would turn one
/// failure into a cascade.
#[cfg(test)]
fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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
    //
    // `std::time::Instant` is `CLOCK_MONOTONIC` on Linux, so it is tempting to
    // read the anchor through it. That does not work: `Instant` deliberately
    // exposes only a *duration*, and lining this trace up with another process's
    // log is exactly the case that needs the absolute value. Two `Instant`s
    // measured here differ by their span, not by their position on the shared
    // clock, so a dump anchored with one cannot be compared with a client-side
    // timestamp at all. The raw read is what makes this record mean something
    // across a process boundary, and `valid=` is kept because the call can fail
    // and a trace that silently reported a zero anchor would be worse than one
    // that says it could not read the clock.
    let mut timestamp = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    record("clock_anchor_before", format_args!(""));
    // SAFETY: `clock_gettime` is a pure read of the kernel's monotonic clock into
    // a caller-owned `timespec`, with no precondition to establish and no pointer
    // the callee retains: `&raw mut timestamp` is the address of a live local that
    // outlives the call, and on the non-zero return path the struct is written
    // before the value is read. The only failure the caller has to handle is the
    // return code, which is checked immediately below.
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

/// Why the trace is being written, and — because the two are the same fact —
/// whether the X half of the shutdown ran.
///
/// The header reports `end=<this>` and `x_teardown=<full|skipped>` from one
/// value rather than two, so the two fields cannot disagree: a dump that said
/// `end=clean_exit x_teardown=skipped` would be claiming a full teardown while
/// admitting nothing was torn down. The only way the X half is skipped is that
/// the connection was gone, so a skipped X half is always reported as a lost
/// connection even when the shutdown had been requested as a clean one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TraceEnd {
    /// The window manager was asked to stop, and the X server was there.
    CleanExit,
    /// The X server died first, so there was nothing left to talk to.
    XConnectionLost,
}

impl TraceEnd {
    /// The token the header carries for this outcome.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::CleanExit => "clean_exit",
            Self::XConnectionLost => "x_connection_lost",
        }
    }

    /// Whether the X half of the shutdown actually ran, which is what
    /// `x_teardown=` reports: `full` for a clean exit, `skipped` for a lost
    /// connection.
    pub(super) fn x_teardown(self) -> &'static str {
        match self {
            Self::CleanExit => "full",
            Self::XConnectionLost => "skipped",
        }
    }
}

impl fmt::Display for TraceEnd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What [`dump`] actually did.
///
/// The dump used to report a failed write with an `eprintln!` of its own and
/// return nothing, so its caller could not say whether the ring buffer had been
/// preserved — which is the only question that matters on the exit path where
/// the buffer is the last remaining evidence of the session. A caller that wants
/// the outcome must look at it; [`dump`] no longer prints, so there is exactly
/// one place that decides how loudly a lost trace is reported.
#[derive(Debug)]
pub(super) struct DumpReport {
    /// True only if the whole file was written and flushed. A partial write is
    /// reported as not written, because a reader cannot tell a truncated dump
    /// from a complete one.
    pub(super) written: bool,
    /// Records that reached the file. Zero when nothing was written.
    pub(super) records: usize,
    /// The file the dump targeted. Empty when tracing was never enabled, which
    /// is why a caller must test `written` or `error` rather than read this.
    pub(super) path: PathBuf,
    /// The `io` error that stopped the write, if any.
    pub(super) error: Option<String>,
}

impl DumpReport {
    /// Nothing to report: tracing was off, so there was never a buffer to write.
    /// A function rather than a constant because an empty `PathBuf` is not
    /// constructible in a `const` at the workspace's MSRV.
    fn not_enabled() -> Self {
        Self {
            written: false,
            records: 0,
            path: PathBuf::new(),
            error: None,
        }
    }
}

/// Write the buffer out and disable tracing for the rest of the process. The
/// header line is the format contract: it pins the clock, the units, how the
/// session ended, and the caveats every reader has to keep in mind — a `swap`
/// that returned is not proof the frame was presented, and a window whose
/// geometry was set to 0 for an off-screen tile is not evidence of a GL
/// present.
///
/// `end` is the outcome being recorded, not a note about it: see [`TraceEnd`].
/// Nothing is printed here — the caller owns the reporting, so a shutdown that
/// knows more than this function does (why the X half was skipped, say) can say
/// it in the same breath as the dump's own result.
pub(super) fn dump(end: TraceEnd) -> DumpReport {
    if !ENABLED.swap(false, Ordering::Relaxed) {
        return DumpReport::not_enabled();
    }
    let Some(buffer) = BUFFER.with(|slot| slot.borrow_mut().take()) else {
        return DumpReport::not_enabled();
    };
    let records = buffer.records.len();
    let path = buffer.path.clone();
    let result = (|| -> io::Result<()> {
        let mut out = io::BufWriter::new(std::fs::File::create(&path)?);
        writeln!(
            out,
            "# maverick_compositor_trace_v1 clock=Instant units=ns x_time=server_ms capacity={} dropped={} truncated={} end={end} x_teardown={} swap_returned_is_not_visible=true off_geometry_is_not_gl_present=true",
            buffer.capacity,
            buffer.dropped,
            buffer.truncated,
            end.x_teardown(),
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
    match result {
        Ok(()) => DumpReport {
            written: true,
            records,
            path,
            error: None,
        },
        Err(error) => DumpReport {
            written: false,
            records,
            path,
            error: Some(error.to_string()),
        },
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
        let _guard = test_lock();
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
        let _guard = test_lock();
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
        let _guard = test_lock();
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

    /// Install a buffer whose dump goes to a file of this test's own, and hand
    /// back that path. The name carries a per-test counter because the buffer is
    /// thread-local but the filesystem is not, and two tests dumping at once
    /// must not read each other's file.
    fn init_buffer_dumping_to(name: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "maverick-trace-{}-{unique}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        BUFFER.with(|slot| {
            *slot.borrow_mut() = Some(Buffer {
                start: Instant::now(),
                path: path.clone(),
                records: Vec::with_capacity(16),
                turn: 0,
                frame: 0,
                dropped: 0,
                truncated: 0,
                last_frame: None,
                capacity: 16,
            });
        });
        ENABLED.store(true, Ordering::Relaxed);
        path
    }

    /// Every dump must say how the session ended, whichever end it was.
    ///
    /// This is the header a reader opens the file for: a trace that cannot say
    /// whether the X half of the shutdown ran is exactly the trace that is
    /// needed when the X server died. Each end is checked on its own instead of
    /// against a table spelled out twice, so an edit that moves one token cannot
    /// move its expectation with it.
    #[test]
    fn the_header_says_how_the_session_ended() {
        for (end, token, teardown) in [
            (TraceEnd::CleanExit, "clean_exit", "full"),
            (TraceEnd::XConnectionLost, "x_connection_lost", "skipped"),
        ] {
            let _guard = test_lock();
            let path = init_buffer_dumping_to("end");
            record("probe", format_args!("i=0"));
            let report = dump(end);
            assert!(report.written, "{token}: {report:?}");
            assert_eq!(report.records, 1, "{token}");
            let header = std::fs::read_to_string(&path).expect("dump was written");
            let header = header.lines().next().expect("a header line");
            assert!(
                header.contains(&format!("end={token} x_teardown={teardown}")),
                "header for {token} does not report the outcome: {header}"
            );
            let _ = std::fs::remove_file(&path);
        }
    }

    /// A dump that could not write says so, and says where it tried, instead of
    /// being indistinguishable from tracing that was never switched on.
    #[test]
    fn a_dump_that_cannot_write_reports_where_and_why() {
        let _guard = test_lock();
        let path = init_buffer_dumping_to("unwritable");
        // A directory is not a file: `File::create` on it fails on every write.
        std::fs::create_dir_all(&path).expect("temp dir is usable");
        let report = dump(TraceEnd::CleanExit);
        assert!(!report.written, "a failed write must not report as written");
        assert!(
            report.error.is_some(),
            "a failed write must carry its error"
        );
        assert_eq!(report.path, path, "a failed write must name its target");
        std::fs::remove_dir(&path).expect("temp dir is removable");
    }

    /// With tracing off there is no buffer and nothing to say, which is not an
    /// error: the report has to distinguish "never enabled" from "tried and
    /// failed" so a caller can stay quiet about the common case.
    #[test]
    fn a_dump_with_tracing_off_reports_nothing_and_no_error() {
        let _guard = test_lock();
        ENABLED.store(false, Ordering::Relaxed);
        BUFFER.with(|slot| *slot.borrow_mut() = None);
        let report = dump(TraceEnd::CleanExit);
        assert!(!report.written);
        assert_eq!(report.records, 0);
        assert_eq!(report.path, PathBuf::new(), "no file was ever targeted");
        assert!(report.error.is_none(), "never enabled is not a failure");
    }
}

/// Property-based coverage of the bounded trace ring buffer.
///
/// The example tests above pin one burst shape each. The properties below
/// generalise them over arbitrary record counts, payload sizes and turn/frame
/// interleavings: the two ways this buffer can lose information — the capacity
/// drop and the payload truncation — both carry accounting that has to stay
/// exact, and both are reached by counts no hand-written example would pick.
#[cfg(test)]
mod property_tests {
    use super::*;
    use proptest::prelude::*;

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        test_lock()
    }

    /// Install a buffer with an explicit capacity, leaving the thread-local slot
    /// empty beforehand. Mirrors the sibling `tests` helper, which is private to
    /// that module and so not reachable from here.
    fn init_buffer_with_capacity(capacity: usize) {
        BUFFER.with(|slot| {
            *slot.borrow_mut() = None;
            *slot.borrow_mut() = Some(Buffer {
                start: Instant::now(),
                path: std::env::temp_dir().join("maverick-trace-property"),
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

    /// Detach the buffer and turn tracing off, returning it for inspection.
    fn take() -> Buffer {
        ENABLED.store(false, Ordering::Relaxed);
        BUFFER
            .with(|slot| slot.borrow_mut().take())
            .expect("buffer must be initialised")
    }

    /// Decode a record's payload. A truncated payload must still be valid UTF-8,
    /// so a failure here is the bug, not a bad test.
    fn payload(r: &Record) -> String {
        std::str::from_utf8(&r.bytes[..r.len])
            .expect("truncation must never split a UTF-8 character")
            .to_string()
    }

    /// One traced turn or frame boundary.
    #[derive(Debug, Clone, Copy)]
    enum Op {
        Turn,
        Frame,
    }

    fn arb_ops() -> impl Strategy<Value = Vec<Op>> {
        prop::collection::vec(prop_oneof![Just(Op::Turn), Just(Op::Frame)], 0..24)
    }

    /// Payload text drawn two ways.
    ///
    /// The first branch is any length at all, so payloads that comfortably fit
    /// are covered and must not be counted as truncated. The second pins the
    /// leading run of ASCII just short of the cut and clusters arbitrary
    /// characters across it, so the boundary walk is exercised on nearly every
    /// generated case rather than only on the rare one where a multi-byte
    /// character happens to straddle `PAYLOAD`.
    fn arb_payload() -> impl Strategy<Value = String> {
        prop_oneof![
            proptest::collection::vec(any::<char>(), 0..200)
                .prop_map(|c| c.into_iter().collect::<String>()),
            (
                (PAYLOAD - 84..PAYLOAD),
                proptest::collection::vec(any::<char>(), 1..40),
                0usize..200usize,
            )
                .prop_map(|(lead, head, trail)| {
                    let mut s = "x".repeat(lead);
                    s.extend(head);
                    s.push_str(&"x".repeat(trail));
                    s
                }),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Past the cap the *earliest* records survive and every later one is
        /// counted as dropped, so the counts printed in the dump header always
        /// account for every record the trace was handed.
        #[test]
        fn overflow_keeps_the_head_and_counts_everything_else(
            n in 0usize..48,
            capacity in 0usize..12,
        ) {
            let _guard = lock();
            init_buffer_with_capacity(capacity);
            for i in 0..n {
                record("probe", format_args!("i={i}"));
            }
            let buffer = take();
            let kept = n.min(capacity);
            prop_assert_eq!(buffer.records.len(), kept);
            prop_assert_eq!(buffer.dropped, (n - kept) as u64);
            // The survivors are the head of the stream, in arrival order: a trace
            // whose head was overwritten cannot explain why the burst started.
            for (k, r) in buffer.records.iter().enumerate() {
                prop_assert_eq!(r.event, "probe");
                prop_assert_eq!(payload(r), format!("i={k}"));
            }
        }

        /// An over-long payload is truncated rather than dropped, never exceeds
        /// the fixed buffer, and never ends mid-code-point: a dump whose last
        /// line held half a character would be unreadable to every tool pointed
        /// at it, and `truncated` must count exactly the records that lost data.
        #[test]
        fn an_over_long_payload_is_cut_at_a_character_boundary(text in arb_payload()) {
            let _guard = lock();
            init_buffer_with_capacity(1);
            record("probe", format_args!("{text}"));
            let buffer = take();
            prop_assert_eq!(buffer.records.len(), 1);
            let r = &buffer.records[0];
            prop_assert!(r.len <= PAYLOAD, "payload overflowed the fixed buffer");
            let stored = payload(r);
            prop_assert!(
                text.starts_with(&stored),
                "stored payload is not a prefix of the text that was recorded"
            );
            prop_assert_eq!(buffer.truncated, u64::from(stored.len() < text.len()));
        }

        /// A dropped record is dropped before its payload is ever written, so the
        /// two counters partition the stream: every record is either stored whole
        /// or stored truncated, never both counted and never neither.
        #[test]
        fn the_drop_and_truncation_counters_never_double_count(
            n in 0usize..32,
            capacity in 1usize..8,
            text in arb_payload(),
        ) {
            let _guard = lock();
            init_buffer_with_capacity(capacity);
            for _ in 0..n {
                record("probe", format_args!("{text}"));
            }
            let buffer = take();
            let stored = buffer.records.len() as u64;
            let truncated = buffer
                .records
                .iter()
                .filter(|r| r.len < text.len())
                .count() as u64;
            prop_assert_eq!(buffer.truncated, truncated);
            prop_assert_eq!(stored + buffer.dropped, n as u64);
        }

        /// The counter a record is stamped with and the marker record emitted for
        /// it are bumped together inside one guard, so the two can never
        /// disagree. The counters are lifetime totals while `records` is a
        /// bounded ring, so they only have to line up while nothing was dropped —
        /// past the cap the counter is allowed to run ahead of what survived, but
        /// it must never fall *behind* the records, which would mean a marker was
        /// emitted without its increment. Both only ever increase, so the stamps
        /// also stay non-decreasing down the buffer, including after an overflow
        /// has started overwriting its head.
        #[test]
        fn counters_and_their_marker_records_cannot_desynchronise(
            ops in arb_ops(),
            capacity in 1usize..=8,
        ) {
            let _guard = lock();
            init_buffer_with_capacity(capacity);
            for op in &ops {
                // `ENABLED` is process-global while the buffer is thread-local.
                // Re-asserting it immediately before each call keeps another
                // test's `take()` from silently skipping a boundary mid-sequence.
                ENABLED.store(true, Ordering::Relaxed);
                match op {
                    Op::Turn => begin_turn(),
                    Op::Frame => begin_frame(),
                }
                record("probe", format_args!("probe"));
            }
            let buffer = take();
            let wanted_turns = ops.iter().filter(|o| matches!(o, Op::Turn)).count() as u64;
            let wanted_frames = ops.iter().filter(|o| matches!(o, Op::Frame)).count() as u64;
            let turns = buffer
                .records
                .iter()
                .filter(|r| r.event == "turn_begin")
                .count() as u64;
            let frames = buffer
                .records
                .iter()
                .filter(|r| r.event == "frame_begin")
                .count() as u64;
            if buffer.dropped == 0 {
                prop_assert_eq!(buffer.turn, wanted_turns);
                prop_assert_eq!(buffer.frame, wanted_frames);
                prop_assert_eq!(buffer.turn, turns);
                prop_assert_eq!(buffer.frame, frames);
            }
            prop_assert!(
                buffer.turn >= turns,
                "a turn was recorded without its counter being bumped"
            );
            prop_assert!(
                buffer.frame >= frames,
                "a frame was recorded without its counter being bumped"
            );
            prop_assert!(buffer.turn <= wanted_turns, "turn counter over-incremented");
            prop_assert!(buffer.frame <= wanted_frames, "frame counter over-incremented");
            for w in buffer.records.windows(2) {
                prop_assert!(w[0].turn <= w[1].turn, "turn stamps went backwards");
                prop_assert!(w[0].frame <= w[1].frame, "frame stamps went backwards");
                prop_assert!(w[0].ns <= w[1].ns, "records must stay in arrival order");
            }
        }

        /// The frame interval is measured between *begins* and there is no
        /// previous begin on the first one, so the first `frame_begin` records
        /// `None` and every later one `Some`. The measured value itself is
        /// wall-clock and is deliberately not asserted — only the shape, which is
        /// what anything parsing the dump actually depends on.
        #[test]
        fn only_the_first_frame_reports_a_missing_interval(frames in 1u32..8) {
            let _guard = lock();
            init_buffer_with_capacity(64);
            for _ in 0..frames {
                ENABLED.store(true, Ordering::Relaxed);
                begin_frame();
            }
            let buffer = take();
            let begins: Vec<&Record> = buffer
                .records
                .iter()
                .filter(|r| r.event == "frame_begin")
                .collect();
            prop_assert_eq!(begins.len() as u32, frames);
            prop_assert!(payload(begins[0]).contains("previous_begin_interval_ns=None"));
            for r in &begins[1..] {
                prop_assert!(
                    payload(r).contains("previous_begin_interval_ns=Some("),
                    "a later frame reported no interval"
                );
            }
        }
    }
}
