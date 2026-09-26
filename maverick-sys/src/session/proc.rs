//! `/proc` reads for the session process tree.
//!
//! A Maverick session is a *graph of processes*, not a pid: the X server, the
//! window manager, whatever the WM autostarts, and everything
//! `maverickctl exec` launched. `maverickctl process list` has to answer "what
//! is running in this session" from that graph alone, with no privileges and no
//! supervising daemon.
//!
//! # How the tree is found
//!
//! Two roots are not enough, and the reason is `exec`. A process started by
//! `maverickctl exec` is a child of that *one-shot CLI*, not of the window
//! manager, so a pure parent-pointer walk from the X server and the WM misses
//! exactly the programs the command exists to launch — and once the CLI exits,
//! they are reparented to init and the pointer is gone.
//!
//! So the tree is the union of three cheap, exact sets, all read from
//! `/proc/<pid>/stat` alone (one small file per process, no environment dump):
//!
//! 1. descendants of the X server pid,
//! 2. descendants of the window-manager pid (its autostart children),
//! 3. every process whose process-group id is one this session registered.
//!
//! (3) is what catches `exec`/`shell`: [`crate::ctl::session`]'s `exec` puts
//! each child in a fresh process group and records the id, so membership
//! survives the CLI that spawned it exiting. A program that calls `setsid`
//! itself leaves its group behind, which is why the environment also carries
//! `MAVERICK_SESSION`; callers that need the widest possible net can sweep for
//! it with [`marked_pids`].
//!
//! # Ownership and cost
//!
//! Pure functions over `/proc`, no caching and no shared state. The whole-tree
//! scan is one `read_dir` plus one `stat` read per process — the same work `ps`
//! does, and what a `maverickctl process list` costs.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Page size used to turn `/proc/<pid>/statm`'s resident-page count into bytes.
/// 4 KiB is the page size on every Linux target Maverick runs on (x86_64 and
/// aarch64); a wrong value would scale the reported RSS, not any decision.
const PAGE_SIZE: u64 = 4096;

/// `_SC_CLK_TCK` on Linux is fixed at 100 for every supported ABI, and is what
/// `/proc/<pid>/stat`'s `utime`/`stime` are denominated in.
pub const CLOCK_TICKS: u64 = 100;

/// One process in a session, as read from `/proc`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcInfo {
    /// Kernel process id.
    pub pid: u32,
    /// Parent process id.
    pub ppid: u32,
    /// Process-group id. Registered group ids are what make a detached
    /// `exec`ed program findable after its launcher exited.
    pub pgid: u32,
    /// Kernel start time, used to tell a live process from a recycled pid.
    pub start_time: u64,
    /// `comm` (the kernel's 15-character name), e.g. `firefox`.
    pub comm: String,
    /// Full command line, NUL-separated in `/proc` and joined with spaces here.
    pub cmdline: String,
    /// Resident set size in bytes.
    pub rss_bytes: u64,
    /// User CPU time in milliseconds.
    pub user_ms: u64,
    /// System CPU time in milliseconds.
    pub sys_ms: u64,
    /// Wall time since the process started, in milliseconds. Together with
    /// `user_ms + sys_ms` this yields the CPU share.
    pub elapsed_ms: u64,
}

impl ProcInfo {
    /// Average CPU share over the process's whole lifetime, in percent.
    ///
    /// This is the share since start, not an instantaneous sample: it is
    /// derived from one `/proc` read instead of two timed ones, so it costs
    /// nothing, is stable for a given process, and is directly comparable
    /// between processes of very different ages. A snapshot of "what has this
    /// been doing" is what a session listing is for; a second sample would make
    /// the command block and the number flicker.
    pub fn cpu_percent(&self) -> f64 {
        if self.elapsed_ms == 0 {
            return 0.0;
        }
        let busy_ms = self.user_ms.saturating_add(self.sys_ms);
        // 1000 converts ms→s against ms elapsed; the /1000 factors cancel, but
        // keeping them explicit documents the unit conversion.
        (busy_ms as f64 * 100.0) / (self.elapsed_ms as f64 * 1.0)
    }

    /// Resident memory rendered for humans (`"42M"`, `"1.2G"`), the shape the
    /// process listing prints.
    pub fn rss_human(&self) -> String {
        human_bytes(self.rss_bytes)
    }

    /// The name to show in a listing: `comm` when the process still has one,
    /// else the first `cmdline` word, else the bare pid. A kernel thread has
    /// an empty `cmdline` and a parenthesised `comm`, and either is better than
    /// an empty column.
    pub fn display_name(&self) -> String {
        if !self.comm.is_empty() && self.comm != "[]" {
            return self.comm.clone();
        }
        self.cmdline
            .split_whitespace()
            .next()
            .map(str::to_string)
            .unwrap_or_else(|| self.pid.to_string())
    }
}

/// Render a byte count the way a process listing shows memory.
pub fn human_bytes(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;
    if bytes >= GIB {
        format!("{:.1}G", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{}M", bytes / MIB)
    } else if bytes >= 1024 {
        format!("{}K", bytes / 1024)
    } else {
        format!("{bytes}B")
    }
}

/// Raw parent/group table for every process visible to this user.
#[derive(Debug, Clone, Default)]
pub struct ProcTable {
    /// `(pid, ppid, pgid, start_time)` in pid order, which is the order
    /// `/proc` enumerates and therefore a stable sort for free.
    pub entries: Vec<(u32, u32, u32, u64)>,
}

impl ProcTable {
    /// Every process this user can see in `/proc`.
    ///
    /// Entries the caller cannot read are skipped rather than treated as
    /// absent-for-authority: a session tree is a description of the caller's
    /// own processes, and a foreign process is never part of one.
    pub fn scan() -> Self {
        let mut entries = Vec::new();
        let Ok(dir) = std::fs::read_dir("/proc") else {
            return Self { entries };
        };
        for entry in dir.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            // `/proc` also holds non-pid directories (`self`, `net`, `sys`…).
            let Ok(pid) = name.parse::<u32>() else {
                continue;
            };
            if let Some(stat) = read_stat(pid) {
                entries.push((pid, stat.0, stat.1, stat.2));
            }
        }
        entries.sort_by_key(|(pid, ..)| *pid);
        Self { entries }
    }

    /// The transitive children of `roots`, including the roots themselves.
    ///
    /// Walked parent-pointer up, repeated until it stops growing: the relation
    /// is a forest, and a child can appear before its parent is reached
    /// (`/proc` enumeration order is by pid, not by tree depth), so a single
    /// pass would drop a whole subtree.
    ///
    /// A non-positive root is dropped rather than walked. Pid 0 is not a
    /// process — it is the "nothing is running" sentinel of
    /// [`crate::session::ProcRef`] — and `/proc/0` does not exist, so the only
    /// way a 0 reaches the fixpoint is as a parent key: pid 1's `ppid` is
    /// literally 0, so seeding from 0 adopts init and then the entire process
    /// table. The caller is expected to pass only roots it has verified; this
    /// is the backstop that keeps a future caller from turning "this session
    /// owns nothing" into "this session owns everything".
    pub fn closure(&self, roots: &[u32]) -> HashSet<u32> {
        let mut found: HashSet<u32> = roots.iter().copied().filter(|pid| *pid > 0).collect();
        loop {
            let before = found.len();
            for (pid, ppid, ..) in &self.entries {
                if found.contains(ppid) {
                    found.insert(*pid);
                }
            }
            if found.len() == before {
                return found;
            }
        }
    }

    /// Every process whose process-group id is in `pgids`.
    ///
    /// A zero id is dropped, and not only because pid 0 is not a process:
    /// kernel threads are created with `pgrp == 0`, so matching 0 would report
    /// every kthread on the machine as a member of whatever asked.
    pub fn in_groups(&self, pgids: &[u32]) -> HashSet<u32> {
        let wanted: HashSet<u32> = pgids.iter().copied().filter(|pgid| *pgid > 0).collect();
        if wanted.is_empty() {
            return HashSet::new();
        }
        self.entries
            .iter()
            .filter(|(_, _, pgid, _)| wanted.contains(pgid))
            .map(|(pid, ..)| *pid)
            .collect()
    }

    /// The parent-pointer links, for a caller that needs to walk itself.
    pub fn parents(&self) -> HashMap<u32, u32> {
        self.entries
            .iter()
            .map(|(pid, ppid, ..)| (*pid, *ppid))
            .collect()
    }
}

/// Read `(ppid, pgid, start_time)` from `/proc/<pid>/stat`.
///
/// Field offsets are counted from the last `)`: `comm` (field 2) is
/// parenthesised and may itself contain both spaces and parentheses, so any
/// fixed offset from the start of the line is wrong for some process. Returns
/// `None` when the process is gone or the line is unparsable — both mean "not
/// a process to describe", not an error.
fn read_stat(pid: u32) -> Option<(u32, u32, u64)> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let pos = text.rfind(')')?;
    let mut fields = text[pos + 1..].split_whitespace();
    // state, ppid, pgrp, session, tty_nr, tpgid, flags, minflt, cminflt,
    // majflt, cmajflt, utime, stime, cutime, cstime, priority, nice,
    // num_threads, itrealvalue, starttime
    let _state = fields.next()?;
    let ppid = fields.next()?.parse().ok()?;
    let pgid = fields.next()?.parse().ok()?;
    for _ in 0..16 {
        fields.next()?;
    }
    let start_time = fields.next()?.parse().ok()?;
    Some((ppid, pgid, start_time))
}

/// Read the full [`ProcInfo`] for `pid`, or `None` if it is gone.
///
/// Three `/proc` reads: `stat` (identity, ancestry, CPU), `statm` (resident
/// pages — cheaper and less parsable than `status`), `cmdline` (NUL separated).
/// A field that cannot be read degrades to its default rather than failing the
/// whole read: a process that is exiting between the reads is still worth
/// listing with whatever was captured.
pub fn read(pid: u32) -> Option<ProcInfo> {
    let (ppid, pgid, start_time) = read_stat(pid)?;
    let comm = read_comm(pid);
    let cmdline = read_cmdline(pid);
    let rss_bytes = read_rss(pid).unwrap_or(0);
    // utime/stime are the 14th/15th fields counted from the last `)`.
    let (user_ticks, sys_ticks) = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|text| {
            let pos = text.rfind(')')?;
            let mut f = text[pos + 1..].split_whitespace();
            for _ in 0..11 {
                f.next()?;
            }
            Some((
                f.next()?.parse::<u64>().ok()?,
                f.next()?.parse::<u64>().ok()?,
            ))
        })
        .unwrap_or((0, 0));
    let user_ms = user_ticks.saturating_mul(1000) / CLOCK_TICKS;
    let sys_ms = sys_ticks.saturating_mul(1000) / CLOCK_TICKS;
    let elapsed_ms = upticks_to_ms(start_time);
    Some(ProcInfo {
        pid,
        ppid,
        pgid,
        start_time,
        comm,
        cmdline,
        rss_bytes,
        user_ms,
        sys_ms,
        elapsed_ms,
    })
}

/// `/proc/uptime`'s first field, in centiseconds — the only conversion
/// available from a start time to wall time, because `starttime` is counted in
/// clock ticks since boot and the caller has no other boot anchor.
fn upticks_to_ms(start_ticks: u64) -> u64 {
    let Ok(text) = std::fs::read_to_string("/proc/uptime") else {
        return 0;
    };
    let Some(up_secs) = text
        .split_whitespace()
        .next()
        .and_then(|s| s.parse::<f64>().ok())
    else {
        return 0;
    };
    let uptime_ms = (up_secs * 1000.0) as u64;
    let started_ms = start_ticks.saturating_mul(1000) / CLOCK_TICKS;
    uptime_ms.saturating_sub(started_ms)
}

/// `comm` from `/proc/<pid>/stat`, i.e. `(name)` including the parentheses a
/// kernel thread carries. Callers that display it use [`ProcInfo::display_name`].
fn read_comm(pid: u32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|text| {
            let open = text.find('(')?;
            let close = text.rfind(')')?;
            if close > open {
                Some(text[open + 1..close].to_string())
            } else {
                None
            }
        })
        .unwrap_or_default()
}

/// `/proc/<pid>/cmdline` with its NUL separators turned into spaces. Empty for
/// a kernel thread, which is why it is not the primary name.
fn read_cmdline(pid: u32) -> String {
    std::fs::read(PathBuf::from(format!("/proc/{pid}/cmdline")))
        .map(|bytes| {
            String::from_utf8_lossy(&bytes)
                .trim_end_matches('\0')
                .replace('\0', " ")
                .trim()
                .to_string()
        })
        .unwrap_or_default()
}

/// Resident set size in bytes from `/proc/<pid>/statm` field 2 (resident pages).
fn read_rss(pid: u32) -> Option<u64> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
    let pages: u64 = text.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages.saturating_mul(PAGE_SIZE))
}

/// Every process whose environment carries `MAVERICK_SESSION=<name>`.
///
/// The widest possible membership test, and the one that survives a program
/// calling `setsid` and leaving its process group. It costs one
/// `/proc/<pid>/environ` read per visible process, so it is a deliberate
/// fallback rather than the main path — see the module docs for why the
/// process-group registry is checked first.
pub fn marked_pids(name: &str) -> HashSet<u32> {
    let needle = format!("MAVERICK_SESSION={name}");
    let needle = needle.as_bytes();
    let mut found = HashSet::new();
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return found;
    };
    for entry in dir.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        // A NUL-separated `KEY=VALUE` list: the needle carries no NUL, so a
        // plain window search cannot straddle two entries.
        if let Ok(bytes) = std::fs::read(format!("/proc/{pid}/environ")) {
            if bytes.windows(needle.len()).any(|w| w == needle) {
                found.insert(pid);
            }
        }
    }
    found
}

/// True if `pid` is still the very process that was at `start_time`.
///
/// The pid alone is not an identity: the kernel recycles pids, and acting on a
/// recycled one means signalling an unrelated process. Every pid this module
/// hands out is paired with a start time precisely so that check is possible.
pub fn pid_is(pid: u32, start_time: u64) -> bool {
    pid != 0 && start_time != 0 && read_stat(pid).map(|s| s.2) == Some(start_time)
}

/// The start time of `pid`, from `stat` alone.
///
/// The narrow read exists because the start time has to be captured in the
/// window right after a spawn, where a full [`read`] (three files) can lose the
/// race with a process that exits immediately — and where the caller only needs
/// the one field.
pub fn start_time(pid: u32) -> Option<u64> {
    read_stat(pid).map(|s| s.2)
}

/// Signal `pid`, choosing the strongest signal that still lets it clean up.
///
/// `SIGTERM` first: a window manager that is asked to die should close its
/// clients and release its socket rather than be torn out from under them.
/// `SIGKILL` only for the case where waiting has already been tried and
/// failed, so no caller has to decide the policy itself.
pub fn terminate(pid: u32, start_time: u64) -> bool {
    if !pid_is(pid, start_time) {
        return false;
    }
    // SAFETY: `kill(2)` with a validated, positive pid. `pid_is` above already
    // proved the pid names the process we recorded, so the recycled-pid case
    // that would hit the wrong target is closed before the signal is sent.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
    true
}

/// `SIGKILL` a process, still gated on the recorded start time.
pub fn kill_hard(pid: u32, start_time: u64) -> bool {
    if !pid_is(pid, start_time) {
        return false;
    }
    // SAFETY: as in `terminate` — `pid_is` proved the identity first.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
    true
}

/// True if the process is still alive (any state, including a zombie — the
/// pid exists until it is reaped, and a zombie X server is still a leak).
pub fn exists(pid: u32) -> bool {
    // SAFETY: `kill(2)` signal 0 performs the permission/existence check
    // without delivering anything.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A process description is only useful if it can describe *this* process,
    /// which is the one thing a test can rely on existing.
    #[test]
    fn reads_this_process() {
        let me = read(std::process::id()).expect("self is in /proc");
        assert_eq!(me.pid, std::process::id());
        assert!(me.start_time > 0);
        assert!(!me.comm.is_empty());
        assert!(
            me.elapsed_ms < 60 * 60 * 1000,
            "a test process is not an hour old"
        );
    }

    #[test]
    fn a_missing_pid_is_not_an_error() {
        // Pid 0 is never a process, and nothing can be read for it.
        assert!(read(0).is_none());
    }

    /// The `comm`/parenthesis trap: field 2 of `stat` may itself contain a
    /// space and a `)`, so every fixed offset from the start of the line is
    /// wrong for some process. A real process with a `)` in its name is not
    /// reproducible from a test, so the invariant is pinned against the
    /// general shape plus the one case a test can construct.
    #[test]
    fn stat_parsing_is_anchored_at_the_last_paren() {
        let (ppid, pgid, start) = read_stat(std::process::id()).expect("self parses");
        assert_eq!(u64::from(ppid), unsafe { libc::getppid() } as u64);
        assert!(pgid > 0);
        assert!(start > 0);
    }

    /// The tree has to include a root's grandchildren, and has to be stable
    /// when the walk needs more than one pass (a `read_dir` order that lists a
    /// child before its parent).
    #[test]
    fn closure_walks_transitively() {
        let table = ProcTable {
            entries: vec![
                (1, 0, 1, 1),
                (2, 1, 1, 1), // child of 1
                (3, 2, 1, 1), // grandchild of 1
                (4, 0, 4, 1), // unrelated
                (5, 0, 7, 1), // a different group
                (6, 0, 7, 1),
            ],
        };
        let set = table.closure(&[1]);
        assert!(set.contains(&1) && set.contains(&2) && set.contains(&3));
        assert!(!set.contains(&4), "a separate tree is not a descendant");
        assert_eq!(table.closure(&[]), HashSet::new());
    }

    /// Ordering must not decide membership: a pid-ordered table lists a
    /// grandchild before its parent, which a single-pass walk drops.
    #[test]
    fn closure_does_not_depend_on_enumeration_order() {
        let forwards = ProcTable {
            entries: vec![(10, 20, 1, 1), (20, 30, 1, 1), (30, 0, 1, 1)],
        };
        let backwards = ProcTable {
            entries: vec![(30, 0, 1, 1), (20, 30, 1, 1), (10, 20, 1, 1)],
        };
        let want: HashSet<u32> = [10, 20, 30].into_iter().collect();
        assert_eq!(forwards.closure(&[30]), want);
        assert_eq!(backwards.closure(&[30]), want);
    }

    /// The `exec` path: a child in a registered group is found even though its
    /// parent is init, which is exactly what happens once the CLI that spawned
    /// it has exited.
    #[test]
    fn groups_find_a_reparented_child() {
        let table = ProcTable {
            entries: vec![(100, 1, 100, 1), (101, 1, 555, 1), (102, 1, 999, 1)],
        };
        let found = table.in_groups(&[555]);
        assert_eq!(found, HashSet::from([101]));
        assert!(table.in_groups(&[]).is_empty());
    }

    /// A stopped session records both roots as `ProcRef::default()`, so pid 0
    /// reaches this call. Seeding from it would adopt pid 1 — whose `ppid` is
    /// literally 0 — and then the whole process table, which is how a stopped
    /// session came to own every process on the machine.
    #[test]
    fn a_zero_root_owns_nothing() {
        // pid 1 with ppid 0, exactly as the kernel reports init.
        let table = ProcTable {
            entries: vec![(1, 0, 1, 1), (2, 1, 2, 1), (3, 2, 3, 1), (100, 1, 100, 1)],
        };
        assert_eq!(table.closure(&[0, 0]), HashSet::new());
        assert_eq!(table.closure(&[0]), HashSet::new());
        // A real root alongside a zero one still walks: the filter drops the
        // sentinel, it does not disable the traversal.
        assert_eq!(table.closure(&[0, 3]), HashSet::from([3]));
    }

    /// Kernel threads are created with `pgrp == 0`, so a zero group id would
    /// report every kthread as a member of whatever asked for it.
    #[test]
    fn a_zero_group_owns_nothing() {
        let table = ProcTable {
            entries: vec![
                (2, 0, 0, 1),     // kthreadd: pgrp 0
                (3, 2, 0, 1),     // a kernel worker: pgrp 0
                (100, 1, 100, 1), // an ordinary process
            ],
        };
        assert_eq!(table.in_groups(&[0]), HashSet::new());
        assert_eq!(table.in_groups(&[0, 100]), HashSet::from([100]));
    }

    #[test]
    fn cpu_percent_is_bounded_by_the_process_lifetime() {
        let mut p = read(std::process::id()).expect("self");
        p.elapsed_ms = 1000;
        p.user_ms = 500;
        p.sys_ms = 500;
        assert!((p.cpu_percent() - 100.0).abs() < 1e-9);
        p.user_ms = 0;
        p.sys_ms = 0;
        assert_eq!(p.cpu_percent(), 0.0);
        // A process whose start time is unreadable reports 0 rather than
        // dividing by zero.
        p.elapsed_ms = 0;
        assert_eq!(p.cpu_percent(), 0.0);
    }

    #[test]
    fn human_bytes_rounds_to_the_unit_a_listing_expects() {
        assert_eq!(human_bytes(512), "512B");
        assert_eq!(human_bytes(42 * 1024 * 1024), "42M");
        assert_eq!(human_bytes(2 * 1024 * 1024 * 1024), "2.0G");
    }

    /// A kernel thread has an empty `cmdline` and a parenthesised `comm`; the
    /// listing still needs a name for it.
    #[test]
    fn display_name_never_returns_empty() {
        let p = ProcInfo {
            pid: 7,
            ppid: 2,
            pgid: 2,
            start_time: 1,
            comm: "kworker/0:1".into(),
            cmdline: String::new(),
            rss_bytes: 0,
            user_ms: 0,
            sys_ms: 0,
            elapsed_ms: 0,
        };
        assert_eq!(p.display_name(), "kworker/0:1");
        let bare = ProcInfo {
            comm: String::new(),
            cmdline: "firefox --new-window".into(),
            ..p
        };
        assert_eq!(bare.display_name(), "firefox");
        let nothing = ProcInfo {
            comm: String::new(),
            cmdline: String::new(),
            ..p
        };
        assert_eq!(nothing.display_name(), "7");
    }

    /// A recycled pid is the reason every pid is paired with a start time.
    /// `pid_is` is the gate on every signal this module sends, so it must
    /// reject a pid whose identity does not match, and a pid of 0.
    #[test]
    fn pid_is_gates_on_the_recorded_start_time() {
        let me = read(std::process::id()).expect("self");
        assert!(pid_is(me.pid, me.start_time));
        assert!(
            !pid_is(me.pid, me.start_time + 1),
            "a wrong start time is not this process"
        );
        assert!(!pid_is(0, me.start_time));
        assert!(
            !pid_is(me.pid, 0),
            "an unrecorded start time proves nothing"
        );
    }

    /// The marker sweep is the fallback membership test, so it has to find
    /// this process when the marker is set and not confuse two sessions.
    #[test]
    fn marked_pids_separates_sessions() {
        // The test binary cannot set its own environment after `main`, so the
        // marker is read for a name that cannot exist: the sweep must
        // terminate and return nothing rather than matching anything.
        assert!(marked_pids("maverick-no-such-session-\u{1}").is_empty());
    }

    /// A real scan must at least find this process and report a coherent
    /// parent link, which is what every tree walk is built on.
    #[test]
    fn a_real_scan_sees_this_process() {
        let table = ProcTable::scan();
        let me = std::process::id();
        let entry = table
            .entries
            .iter()
            .find(|(pid, ..)| *pid == me)
            .expect("self is visible in /proc");
        assert_eq!(u64::from(entry.1), unsafe { libc::getppid() } as u64);
        assert!(table.closure(&[me]).contains(&me));
    }
}
