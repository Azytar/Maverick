# SYS-AUDIT — is `maverick-sys` justified?

**Agent C** · audit of `/path/to/Maverick-reconstructed/maverick-sys` · read-only; no `.rs`, `Cargo.toml` or `Cargo.lock` was modified and no cargo build/test was run.

---

## 1. Summary

`maverick-sys` is **not one crate**. It is three unrelated things fused behind a single dependency edge: a ~1,100-line OS/FFI boundary (signals, `poll`, uid/gid, peer credentials), a ~7,000-line Maverick **application** layer (instance identity/discovery, the control protocol, the Session model, the `maverickctl` CLI engine), and ~5,800 lines of tests. Of **277 public items**, exactly **27 have any consumer outside the crate**, and **250 have none**.

**Biggest finding: 8,995 of the crate's 13,119 production lines (68.6%) are `session` + `ctl` + `discover` — an X-server session manager and an argument parser, not an OS boundary.** The crate's own `categories = ["os::unix-apis"]` is wrong for 68% of it. The genuine OS boundary is `src/lib.rs` (209 production lines), `control::peer_uid` (5 lines), and `identity::current_uid/current_gid` (10 lines) — **~225 production lines total**, 1.7% of the crate.

The OS/FFI part is **justified and correct**: the manifest's claim that rustix cannot replace `sigaction` is **verified** (`not_implemented!(sigaction)` at rustix-1.1.4 `src/not_implemented.rs:72`), the `unsafe` surface is 4 blocks in production code, and `Signal` owns a real invariant (a handler stores an `AtomicBool`; `SA_NOCLDWAIT` is applied and its refusal is reported). The layering there is right and should be kept.

The problem is everything else. `maverickctl` is a **shipped binary produced by this crate** (`maverick-sys/Cargo.toml` has no `[[bin]]`, but `src/bin/maverickctl.rs` is auto-discovered; `install.sh:150` `RUNTIME_BINS=(maverick maverickctl)` and `install.sh:1478/1483` `cargo build -p maverick -p maverick-sys` prove it). Deleting this crate deletes a user-facing artifact. That is the single most important fact for the campaign's premise.

The `rustix` dependency adds **zero packages** to the shipped `maverick` binary: `Cargo.lock:439-449` shows `x11rb 0.13.2` already depends on `rustix 1.1.4` (features `std, event, fs, net, system`; x11rb-0.13.2 `Cargo.toml:252-260`). `maverick-sys` is the only reason `process` and `rand` are added to the rustix feature set (`maverick-sys/Cargo.toml:28-37`). Removing the crate removes the `process`/`rand` features but **not the rustix package** — so "removing `maverick-sys` removes rustix" is false as stated.

---

## 2. Module map with line counts

Total `maverick-sys`: **14,718 lines** (13,119 in `src/`, 1,599 in `tests/`).

| Module | Total | inline `#[cfg(test)]` | production | public items | concern |
|---|---:|---:|---:|---:|---|
| `src/lib.rs` | 577 | 467 | **110** | 26 | (a) FFI — signals, poll, detach |
| `src/control.rs` | 1,472 | 769 | 703 | 19 | (b) protocol + 5 lines FFI |
| `src/hub.rs` | 421 | 114 | 307 | 15 | (b) thread bridge (pure `std`) |
| `src/identity.rs` | 1,065 | 466 | 599 | 37 | (b) + 10 lines FFI |
| `src/json.rs` | 1,047 | 379 | 668 | 23 | (b) pure codec, no OS at all |
| `src/discover.rs` | 180 | 0 | 180 | 7 | (b) |
| `src/ctl/mod.rs` | 1,260 | 183 | 1,077 | 15 | (b) CLI arg parser |
| `src/ctl/session.rs` | 1,746 | 345 | 1,401 | 14 | (b) |
| `src/ctl/windows.rs` | 889 | 223 | 666 | 8 | (b) |
| `src/session/mod.rs` | 1,662 | 393 | 1,269 | 60 | (b) Session record model |
| `src/session/lifecycle.rs` | 855 | 117 | 738 | 11 | (b) process-graph supervision |
| `src/session/xserver.rs` | 1,129 | 399 | 730 | 23 | (b) + rustix fs/rand |
| `src/session/proc.rs` | 807 | 305 | 502 | 19 | (a/b) `/proc` + signals |
| `src/bin/maverickctl.rs` | 9 | 0 | 9 | — | **shipped binary** |
| `tests/*.rs` (7 files + `common/`) | 1,599 | — | — | — | (c) |

**Production-line breakdown by concern:**

| Concern | Production lines | % of production |
|---|---:|---:|
| (b) domain/application logic (`session`, `ctl`, `discover`, `hub`, `json`, most of `control`/`identity`) | ~8,995 | **68.6%** |
| (a) thin OS/FFI primitives | ~225 | **1.7%** |
| (b) control-protocol/identity plumbing not FFI | ~3,800 | 29.0% |
| (c) tests (inline + `tests/`) | ~5,800 | — |

**By area (production only):** `session/` = 4,239 · `ctl/` = 3,144 · `control` = 703 · `json` = 668 · `identity` = 599 · `hub` = 307 · `discover` = 180 · `lib` = 110.

---

## 3. Full public API table

**277 public items.** Columns: `ext/intl/tst` = number of word-boundary references outside `maverick-sys/src`, inside `maverick-sys/src` (excluding the definition line), and inside `maverick-sys/tests`. "ext" over-counts for names that collide with other code (`parse`, `get`, `label`, `spawn`, `stop`, `binary`, `Display`) — the authoritative external surface is derived separately in §3.2.

### 3.1 Complete enumeration

| Symbol | Signature (truncated) | file:line | ext/intl/tst |
|---|---|---|---|
| `MAX_SUBSCRIBERS` | `pub const MAX_SUBSCRIBERS: usize = 16;` | `control.rs:86` | 0/11/2 |
| `MAX_LINE_LEN` | `pub const MAX_LINE_LEN: usize = 64 * 1024;` | `control.rs:89` | 0/22/0 |
| `MAX_CMD_LEN` | `pub const MAX_CMD_LEN: usize = 64 * 1024;` | `control.rs:91` | 0/2/4 |
| `peer_uid` | `pub fn peer_uid(stream: &UnixStream) -> io::Result<u32>` | `control.rs:105` | 0/7/1 |
| `ControlServer` | `pub struct ControlServer {` | `control.rs:137` | 7/—/— |
| `ControlServer::owner_uid` | `pub fn owner_uid(&self) -> u32` | `control.rs:150` | 0/30/5 |
| `ControlServer::spawn` | `pub fn spawn(name: &str, identity_json: String, hub: ControlHub) -> io::Result<Self>` | `control.rs:175` | 2/—/— |
| `ControlServer::shutdown` | `pub fn shutdown(&self)` | `control.rs:282` | 0/—/— |
| `send_command` | `pub fn send_command(name: &str, cmd: &str) -> io::Result<String>` | `control.rs:542` | 0/13/6 |
| `ping` | `pub fn ping(name: &str) -> io::Result<String>` | `control.rs:592` | 0/—/— |
| `identify` | `pub fn identify(name: &str) -> io::Result<String>` | `control.rs:597` | 0/—/— |
| `quit` | `pub fn quit(name: &str) -> io::Result<String>` | `control.rs:602` | 0/—/— |
| `restart` | `pub fn restart(name: &str) -> io::Result<String>` | `control.rs:607` | 0/—/— |
| `reload` | `pub fn reload(name: &str) -> io::Result<String>` | `control.rs:612` | 0/—/— |
| `state` | `pub fn state(name: &str) -> io::Result<String>` | `control.rs:617` | 0/—/— |
| `dispatch` | `pub fn dispatch(name: &str, action: &str) -> io::Result<String>` | `control.rs:623` | 0/—/— |
| `query` | `pub fn query(name: &str, topic: &str) -> io::Result<String>` | `control.rs:636` | 0/—/— |
| `subscribe_stream` | `pub fn subscribe_stream<F>(name: &str, mut on_line: F) -> io::Result<()>` | `control.rs:649` | 0/8/0 |
| `identity_json` | `pub fn identity_json(info: &InstanceInfo) -> String` | `control.rs:687` | 2/—/— |
| `ctl::session` | `pub mod session;` | `ctl/mod.rs:38` | 0/—/— |
| `ctl::windows` | `pub mod windows;` | `ctl/mod.rs:39` | 0/—/— |
| `WindowInfo` (re-export) | `pub use windows::WindowInfo;` | `ctl/mod.rs:41` | 0/—/— |
| `main_with_args` | `pub fn main_with_args(tool: &str, args: Vec<String>) -> ExitCode` | `ctl/mod.rs:80` | 0/2/10 |
| `Ctl` | `pub struct Ctl {` | `ctl/mod.rs:173` | 0/44/1 |
| `Ctl::parse` | `pub fn parse(tool: &str, args: &[String], keep: &[&str]) -> Self` | `ctl/mod.rs:~200` | 0/—/— |
| `Ctl::tool` | `pub fn tool(&self) -> &str` | `ctl/mod.rs:~256` | 0/—/— |
| `Ctl::is_own_flag` | `pub fn is_own_flag(&self, arg: &str) -> bool` | `ctl/mod.rs:266` | 0/3/0 |
| `Ctl::flag` | `pub fn flag(&self, names: &[&str]) -> bool` | `ctl/mod.rs:~270` | 0/—/— |
| `Ctl::explicit_session` | `pub fn explicit_session(&self) -> Option<&str>` | `ctl/mod.rs:276` | 0/5/0 |
| `Ctl::stderr_is_tty` | `pub fn stderr_is_tty(&self) -> bool` | `ctl/mod.rs:282` | 0/2/0 |
| `Usage` | `pub enum Usage {` | `ctl/mod.rs:~340` | 0/—/— |
| `print_usage` | `pub fn print_usage(which: Usage)` | `ctl/mod.rs:353` | 0/8/0 |
| `session_target` | `pub fn session_target(c: &Ctl, args: &[String]) -> Result<String, String>` | `ctl/mod.rs:460` | 0/14/0 |
| `dispatch_to` | `pub fn dispatch_to(view: &SessionView, action: &str) -> Result<(), String>` | `ctl/mod.rs:474` | **0/0/0** |
| `session::run` | `pub fn run(c: &mut Ctl, args: &[String]) -> Result<bool, String>` | `ctl/session.rs:~35` | 0/—/— |
| `session::exec` | `pub fn exec(c: &Ctl, args: &[String]) -> Result<(), String>` | `ctl/session.rs:~60` | 0/—/— |
| `session::shell` | `pub fn shell(c: &Ctl, args: &[String]) -> Result<(), String>` | `ctl/session.rs:~140` | 0/—/— |
| `session::attach` | `pub fn attach(c: &Ctl, args: &[String]) -> Result<(), String>` | `ctl/session.rs:~300` | 0/—/— |
| `split_session_and_rest` | `pub fn split_session_and_rest(c: &Ctl, args: &[String]) -> Option<(String, Vec<String>)>` | `ctl/session.rs:644` | 0/5/0 |
| `Call` | `pub struct Call {` | `ctl/session.rs:~710` | 0/—/— |
| `split_call` | `pub fn split_call(c: &Ctl, args: &[String]) -> Option<Call>` | `ctl/session.rs:725` | 0/7/0 |
| `process` | `pub fn process(c: &mut Ctl, args: &[String]) -> Result<bool, String>` | `ctl/session.rs:~810` | 0/—/— |
| `logs` | `pub fn logs(c: &Ctl, args: &[String]) -> Result<(), String>` | `ctl/session.rs:~880` | 0/—/— |
| `debug` | `pub fn debug(c: &Ctl, args: &[String]) -> Result<(), String>` | `ctl/session.rs:~1040` | 0/—/— |
| `inspect` | `pub fn inspect(c: &Ctl, args: &[String]) -> Result<(), String>` | `ctl/session.rs:~1100` | 0/—/— |
| `truncate` | `pub fn truncate(s: &str, max: usize) -> String` | `ctl/session.rs:~1370` | 0/—/— |
| `live_record` | `pub fn live_record(name: &str) -> Option<Session>` | `ctl/session.rs:1379` | 0/11/0 |
| `available_sessions` | `pub fn available_sessions() -> String` | `ctl/session.rs:1386` | 0/10/0 |
| `WindowInfo` | `pub struct WindowInfo {` | `ctl/windows.rs:~25` | 0/—/— |
| `flatten_windows` | `pub fn flatten_windows(tree: &Json) -> Vec<WindowInfo>` | `ctl/windows.rs:66` | 0/8/0 |
| `resolve_window` | `pub fn resolve_window(windows: &[WindowInfo], selector: &str) -> Result<u32, String>` | `ctl/windows.rs:177` | 0/16/0 |
| `window_selector` | `pub(crate) fn window_selector(c: &Ctl, args: &[String]) -> Option<String>` | `ctl/windows.rs:503` | 0/8/0 |
| `windows::run` | `pub fn run(c: &mut Ctl, args: &[String]) -> Result<bool, String>` | `ctl/windows.rs:~265` | 0/—/— |
| `camera` | `pub fn camera(c: &Ctl, args: &[String]) -> Result<(), String>` | `ctl/windows.rs:~550` | 0/—/— |
| `resize` | `pub fn resize(c: &Ctl, args: &[String]) -> Result<(), String>` | `ctl/windows.rs:~578` | 0/—/— |
| `layout` | `pub fn layout(c: &Ctl, args: &[String]) -> Result<(), String>` | `ctl/windows.rs:~630` | 0/—/— |
| `list_instances` | `pub fn list_instances() -> Vec<InstanceInfo>` | `discover.rs:41` | 0/10/0 |
| `find_by_name` | `pub fn find_by_name(name: &str) -> Option<InstanceInfo>` | `discover.rs:114` | 0/4/0 |
| `find_by_display` | `pub fn find_by_display(display: &str) -> Vec<InstanceInfo>` | `discover.rs:121` | **0/0/0** |
| `quit_by_name` | `pub fn quit_by_name(sid: &str) -> io::Result<String>` | `discover.rs:141` | 0/4/0 |
| `quit_all` | `pub fn quit_all() -> Vec<(String, io::Result<String>)>` | `discover.rs:147` | 0/2/0 |
| `prune_stale` | `pub fn prune_stale() -> Vec<String>` | `discover.rs:163` | 0/2/0 |
| `ensure_runtime_dir` | `pub fn ensure_runtime_dir() -> io::Result<()>` | `discover.rs:175` | 0/7/0 — **dead: all 7 hits are `identity::ensure_runtime_dir` or the definition; the `discover` one is never called** |
| `CMD_CAP` | `pub const CMD_CAP: usize = 128;` | `hub.rs:61` | 0/6/3 |
| `SUB_CAP` | `pub const SUB_CAP: usize = 64;` | `hub.rs:67` | 0/9/4 |
| `ControlCommand` | `pub enum ControlCommand { Quit, Restart, Reload, Dispatch, Query }` | `hub.rs:76` | 5/—/— |
| `ControlHub` | `pub struct ControlHub {` | `hub.rs:99` | 7/—/— |
| `ControlHub::new` | `pub fn new() -> Self` | `hub.rs:126` | 2/—/— |
| `push_command` | `pub fn push_command(&self, cmd: ControlCommand) -> bool` | `hub.rs:153` | 0/16/4 |
| `wake_fd` | `pub fn wake_fd(&self) -> RawFd` | `hub.rs:166` | 1/—/— |
| `snapshot` | `pub fn snapshot(&self) -> String` | `hub.rs:189` | 0/—/— |
| `subscribe` | `pub fn subscribe(&self) -> Receiver<String>` | `hub.rs:205` | 0/—/— |
| `try_subscribe` | `pub fn try_subscribe(&self, max: usize) -> Option<Receiver<String>>` | `hub.rs:223` | 0/5/2 |
| `drain_commands` | `pub fn drain_commands(&self) -> Vec<ControlCommand>` | `hub.rs:250` | 1/—/— |
| `publish_state` | `pub fn publish_state(&self, json: impl Into<String>)` | `hub.rs:262` | 1/—/— |
| `emit` | `pub fn emit(&self, line: impl Into<String>)` | `hub.rs:276` | 2/—/— |
| `subscriber_count` | `pub fn subscriber_count(&self) -> usize` | `hub.rs:291` | 0/13/8 |
| `dropped_events` | `pub fn dropped_events(&self) -> usize` | `hub.rs:297` | 0/4/1 |
| `DEFAULT_NAME` | `pub const DEFAULT_NAME: &str = "default";` | `identity.rs:37` | 1/—/— |
| `QUIT_CMD` … `QUERY_CMD` | `pub const …_CMD: &str = "…";` (9 consts) | `identity.rs:40-50` | 0 each |
| `InstanceInfo` | `pub struct InstanceInfo {` (11 pub fields) | `identity.rs:54` | 1/—/— |
| `InstanceInfo::label` | `pub fn label(&self) -> String` | `identity.rs:~95` | 0/—/— |
| `runtime_dir` | `pub fn runtime_dir() -> PathBuf` | `identity.rs:104` | 0/20/1 |
| `current_uid` | `pub fn current_uid() -> u32` | `identity.rs:131` | 0/21/10 |
| `current_gid` | `pub fn current_gid() -> u32` | `identity.rs:143` | 0/8/4 |
| `ensure_runtime_dir` | `pub fn ensure_runtime_dir() -> io::Result<PathBuf>` | `identity.rs:160` | 0/7/0 |
| `MAX_SID_LEN` | `pub const MAX_SID_LEN: usize = 64;` | `identity.rs:170` | 0/3/5 |
| `is_valid_sid` | `pub fn is_valid_sid(sid: &str) -> bool` | `identity.rs:175` | 0/6/9 |
| `session_dir` | `pub fn session_dir(sid: &str) -> PathBuf` | `identity.rs:204` | 0/20/6 |
| `try_session_dir` | `pub fn try_session_dir(sid: &str) -> io::Result<PathBuf>` | `identity.rs:212` | 0/8/3 |
| `sock_path` | `pub fn sock_path(sid: &str) -> PathBuf` | `identity.rs:225` | 0/18/6 |
| `try_sock_path` | `pub fn try_sock_path(sid: &str) -> io::Result<PathBuf>` | `identity.rs:235` | 1/—/— |
| `meta_path` | `pub fn meta_path(sid: &str) -> PathBuf` | `identity.rs:248` | 1/—/— |
| `try_meta_path` | `pub fn try_meta_path(sid: &str) -> io::Result<PathBuf>` | `identity.rs:258` | 0/7/3 |
| `new_session_id` | `pub fn new_session_id() -> String` | `identity.rs:269` | 0/6/2 |
| `set_private_dir` | `pub(crate) fn set_private_dir(path: &Path) -> io::Result<()>` | `identity.rs:293` | 0/11/0 |
| `current_display` | `pub fn current_display() -> String` | `identity.rs:307` | 0/5/0 |
| `current_tty_nr` | `pub fn current_tty_nr() -> u64` | `identity.rs:314` | 0/3/0 |
| `read_proc_environ_display` | `pub fn read_proc_environ_display(pid: u32) -> String` | `identity.rs:322` | 0/2/0 |
| `read_proc_tty` | `pub fn read_proc_tty(pid: u32) -> u64` | `identity.rs:342` | 0/4/0 |
| `read_proc_starttime` | `pub fn read_proc_starttime(pid: u32) -> u64` | `identity.rs:368` | 0/4/0 |
| `read_proc_exe` | `pub fn read_proc_exe(pid: u32) -> String` | `identity.rs:391` | 0/3/0 |
| `write_meta` | `pub fn write_meta(info: &InstanceInfo) -> io::Result<()>` | `identity.rs:408` | 2/—/— |
| `cleanup_meta` | `pub fn cleanup_meta(sid: &str)` | `identity.rs:435` | 4/—/— |
| `self_info` | `pub fn self_info(name: &str) -> InstanceInfo` | `identity.rs:532` | 1/—/— |
| `self_info_with_sid` | `pub fn self_info_with_sid(name: &str, sid: &str) -> io::Result<InstanceInfo>` | `identity.rs:553` | 1/—/— |
| `read_meta` | `pub fn read_meta(sid: &str) -> Option<InstanceInfo>` | `identity.rs:595` | 0/6/0 |
| `json_escape` | `pub fn json_escape(s: &str) -> String` | `json.rs:20` | 11/—/— |
| `json_quote` | `pub fn json_quote(s: &str) -> String` | `json.rs:39` | 0/51/7 |
| `json_unescape` | `pub fn json_unescape(s: &str) -> String` | `json.rs:50` | 0/14/8 |
| `Value<'a>` | `pub enum Value<'a> { … }` | `json.rs:~110` | 0/—/— |
| `Field<'a>` | `pub struct Field<'a> { pub key, pub value }` | `json.rs:~128` | 0/—/— |
| `Field::as_str` | `pub fn as_str(&self) -> Option<String>` | `json.rs:~135` | 0/—/— |
| `Field::text` | `pub fn text(&self) -> String` | `json.rs:~140` | 0/—/— |
| `Field::as_u64` | `pub fn as_u64(&self) -> Option<u64>` | `json.rs:140` | 0/20/0 |
| `Field::as_bool` | `pub fn as_bool(&self) -> Option<bool>` | `json.rs:~152` | 0/—/— |
| `Field::as_str_array` | `pub fn as_str_array(&self) -> Option<Vec<String>>` | `json.rs:161` | 0/6/0 |
| `quote_array` | `pub fn quote_array<I,S>(items: I) -> String` | `json.rs:170` | 0/6/0 |
| `scan_object` | `pub fn scan_object(doc: &str) -> Vec<Field<'_>>` | `json.rs:198` | 0/13/0 |
| `Json` | `pub enum Json { … }` | `json.rs:347` | 2/—/— |
| `Json::get` | `pub fn get(&self, key: &str) -> Option<&Json>` | `json.rs:364` | (test-only) |
| `Json::str_field` | `pub fn str_field(&self, key: &str) -> &str` | `json.rs:372` | 2 |
| `Json::num_field` | `pub fn num_field(&self, key: &str) -> u64` | `json.rs:380` | 7 |
| `Json::bool_field` | `pub fn bool_field(&self, key: &str) -> bool` | `json.rs:385` | 3 |
| `Json::as_array` | `pub fn as_array(&self) -> &[Json]` | `json.rs:390` | 4 |
| `Json::as_f64` | `pub fn as_f64(&self) -> Option<f64>` | `json.rs:~400` | 0/—/— |
| `Json::as_u64` | `pub fn as_u64(&self) -> Option<u64>` | `json.rs:410` | 0/20/0 |
| `Json::as_str` | `pub fn as_str(&self) -> Option<&str>` | `json.rs:~420` | 0/—/— |
| `Json::to_json` | `pub fn to_json(&self) -> String` | `json.rs:432` | 0/9/0 |
| `parse` | `pub fn parse(doc: &str) -> Option<Json>` | `json.rs:473` | 1 (test) |
| `quit_requested` | `pub fn quit_requested() -> bool` | `lib.rs:214` | 1 |
| `clear_quit` | `pub fn clear_quit()` | `lib.rs:220` | 1 |
| `need_regrab` | `pub fn need_regrab() -> bool` | `lib.rs:226` | 1 |
| `clear_regrab` | `pub fn clear_regrab()` | `lib.rs:232` | 1 |
| `Signal` | `pub struct Signal { handlers, ignored }` | `lib.rs:238` | 11 |
| `Signal::new` / `Default` | `pub fn new() -> Self` | `lib.rs:250` | 2 |
| `Signal::ignore` | `pub fn ignore(mut self, sig: libc::c_int) -> Self` | `lib.rs:258` | 1 |
| `Signal::on_sigterm` | `pub fn on_sigterm(mut self, sig: libc::c_int) -> Self` | `lib.rs:273` | 1 |
| `Signal::on_sigcont` | `pub fn on_sigcont(mut self, sig: libc::c_int) -> Self` | `lib.rs:279` | 1 |
| `Signal::install` | `pub fn install(self) -> Vec<libc::c_int>` | `lib.rs:324` | 2 |
| `detach_from_terminal` | `pub fn detach_from_terminal()` | `lib.rs:431` | 3 |
| `wait_readable_fds` | `pub fn wait_readable_fds(fds: &[RawFd], timeout: Option<Duration>) -> bool` | `lib.rs:466` | 1 |
| `mod control/ctl/discover/hub/identity/json/session` | `pub mod …;` (7) | `lib.rs:507-513` | — |
| re-exports | `pub use control::ControlServer; hub::{ControlCommand, ControlHub}; identity::{self_info, InstanceInfo, DEFAULT_NAME}; session::{Session, SessionName, SessionState};` | `lib.rs:515-518` | **`Session`, `SessionName`, `SessionState` re-exports have zero external callers** |
| `prop_support` | `pub(crate) mod prop_support` (cfg(test)) | `lib.rs:523` | 0 |
| `Binary` | `pub struct Binary { pub path: PathBuf }` | `session/lifecycle.rs:58` | 0 |
| `resolve_binary` | `pub fn resolve_binary(binary: &str) -> Result<Binary, SessionError>` | `session/lifecycle.rs:71` | 0/7/0 |
| `create` | `pub fn create(name: &SessionName, spec: Spec) -> Result<Session, SessionError>` | `session/lifecycle.rs:~120` | 0 |
| `start` | `pub fn start(name: &SessionName) -> Result<Session, SessionError>` | `session/lifecycle.rs:~200` | 0 |
| `stop` | `pub fn stop(name: &SessionName) -> Result<Session, SessionError>` | `session/lifecycle.rs:~240` | 0 |
| `restart` | `pub fn restart(name: &SessionName) -> Result<Session, SessionError>` | `session/lifecycle.rs:~260` | 0 |
| `kill` | `pub fn kill(name: &SessionName) -> Result<Session, SessionError>` | `session/lifecycle.rs:~270` | 0 |
| `remove` | `pub fn remove(name: &SessionName, force: bool) -> Result<(), SessionError>` | `session/lifecycle.rs:~272` | 0 |
| `reap` | `pub fn reap() -> Vec<SessionName>` | `session/lifecycle.rs:279` | 0/4/1 |
| `owner` | `pub fn owner() -> (u32, u32)` | `session/lifecycle.rs:~290` | 0 |
| `backend_is_known` | `pub fn backend_is_known(backend: Backend) -> bool` | `session/lifecycle.rs:735` | 0/3/0 — **only hit outside the def is its own test (lifecycle.rs:851,852)** |
| `session::lifecycle/proc/xserver` | `pub mod …;` (3) | `session/mod.rs:57-59` | 0 |
| `Backend`, `Display`, `XServer` (re-export) | `pub use xserver::{Backend, Display, XServer};` | `session/mod.rs:69` | 0 |
| `SPEC_FILE` | `pub const SPEC_FILE: &str = "session.json";` | `session/mod.rs:72` | 0 |
| `MAIN_ALIAS` | `pub const MAIN_ALIAS: &str = "main";` | `session/mod.rs:79` | 0 |
| `MAX_DIMENSION` | `pub const MAX_DIMENSION: u32 = 16_384;` | `session/mod.rs:87` | 0 |
| `Resolution` | `pub struct Resolution { pub width, pub height }` | `session/mod.rs:91` | 0 |
| `Resolution::DEFAULT` | `pub const DEFAULT: Resolution` | `session/mod.rs:102` | 0 |
| `Resolution::new` / `parse` | `pub fn new(w,h) -> io::Result<Self>` / `parse(s) -> io::Result<Self>` | `session/mod.rs:108/120` | 0 |
| `SessionName` | `pub struct SessionName(String);` | `session/mod.rs:158` | 0 |
| `SessionName::parse` / `as_str` | `pub fn parse(name: &str) -> io::Result<Self>` / `as_str` | `session/mod.rs:~165/170` | 0 |
| `ProcRef` | `pub struct ProcRef { pub pid, pub start_time }` | `session/mod.rs:~200` | 0 |
| `ProcRef::of` / `is_alive` / `is_some` | `pub fn …` | `session/mod.rs:~210/217/~222` | 0 |
| `SessionState` | `pub enum SessionState { … }` | `session/mod.rs:236` | 0 |
| `SessionState::as_str/is_live/parse` | `pub fn …` | `session/mod.rs:~245-265` | 0 |
| `Spec` | `pub struct Spec { … 9 pub fields }` | `session/mod.rs:284` | 0 |
| `Session` | `pub struct Session { … 13 pub fields }` | `session/mod.rs:~300` | 0 |
| `Session::wm_is_up` | `pub fn wm_is_up(&self) -> bool` | `session/mod.rs:357` | 0/5/0 |
| `Session::xserver_is_up` | `pub fn xserver_is_up(&self) -> bool` | `session/mod.rs:365` | **0/0/0** |
| `Session::derived_state` | `pub fn derived_state(&self) -> SessionState` | `session/mod.rs:373` | 0/9/0 |
| `Session::new` / `owned_roots` | `pub fn …` | `session/mod.rs:~400/419` | 0 |
| `Session::dir/xauth_path/log_path/xserver_log_path/env` | `pub fn … -> PathBuf` | `session/mod.rs:426-460` | 0 |
| `Session::to_json` / `from_json` | `pub fn to_json(&self) -> String` / `from_json(doc) -> Option<Self>` | `session/mod.rs:502/555` | 0 |
| `epoch_secs` | `pub(crate) fn epoch_secs() -> u64` | `session/mod.rs:624` | 0/2/0 |
| `session_dir/spec_path/xauth_path/log_path/xserver_log_path` | `pub fn …(name: &SessionName) -> PathBuf` (5) | `session/mod.rs:633-653` | 0 |
| `read` | `pub fn read(name: &SessionName) -> Option<Session>` | `session/mod.rs:~665` | 0 |
| `read_checked` | `pub fn read_checked(name: &SessionName) -> Result<Session, SessionError>` | `session/mod.rs:674` | 0/9/2 |
| `write` | `pub fn write(session: &Session) -> io::Result<()>` | `session/mod.rs:~690` | 0 |
| `current_uid` / `current_gid` | `pub fn current_uid() -> u32` / `current_gid()` | `session/mod.rs:726/731` | 0 — **pure re-export of `identity::current_uid/gid` (`session/mod.rs:727, 732`)** |
| `names` | `pub fn names() -> Vec<SessionName>` | `session/mod.rs:~740` | 0 |
| `SessionView` | `pub struct SessionView { … 16 pub fields }` | `session/mod.rs:772` | 0 |
| `SessionView::mode` | `pub fn mode(&self) -> &'static str` | `session/mod.rs:~955` | 0 |
| `list` | `pub fn list() -> Vec<SessionView>` | `session/mod.rs:~835` | 0 |
| `resolve` | `pub fn resolve(name: &str) -> Result<SessionView, SessionError>` | `session/mod.rs:~935` | 0 |
| `main_session` | `pub fn main_session() -> Option<SessionView>` | `session/mod.rs:985` | 0/3/0 |
| `resolve_target` | `pub fn resolve_target(explicit: Option<&str>) -> Result<SessionView, SessionError>` | `session/mod.rs:1034` | 0/12/0 |
| `SessionError` | `pub enum SessionError { … }` | `session/mod.rs:1068` | 0 |
| `START_TIMEOUT` / `STOP_GRACE` | `pub const …: Duration` | `session/mod.rs:1169/1172` | 0 |
| `wait_until` | `pub fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool` | `session/mod.rs:1179` | 0/4/0 |
| `remove_dir` | `pub fn remove_dir(name: &SessionName) -> io::Result<()>` | `session/mod.rs:~1200` | 0 |
| `tail` | `pub fn tail(path: &Path, lines: usize) -> io::Result<Vec<String>>` | `session/mod.rs:~1215` | 0 |
| `CLOCK_TICKS` | `pub const CLOCK_TICKS: u64 = 100;` | `session/proc.rs:47` | 0 |
| `ProcInfo` | `pub struct ProcInfo { … 10 pub fields }` | `session/proc.rs:51` | 0 |
| `ProcInfo::cpu_percent/rss_human/display_name` | `pub fn …` | `session/proc.rs:85/97/105` | 0 |
| `human_bytes` | `pub fn human_bytes(bytes: u64) -> String` | `session/proc.rs:118` | 0/5/0 |
| `ProcTable` | `pub struct ProcTable { pub entries: Vec<(u32,u32,u32,u64)> }` | `session/proc.rs:134` | 0 |
| `ProcTable::scan/closure/in_groups/parents` | `pub fn …` | `session/proc.rs:143/181/202/213` | 0 (`in_groups` 0/6/0) |
| `read` | `pub fn read(pid: u32) -> Option<ProcInfo>` | `session/proc.rs:~310` | 0 |
| `marked_pids` | `pub fn marked_pids(name: &str) -> HashSet<u32>` | `session/proc.rs:365` | 0/3/0 — only the def, 2 doc mentions and 1 test assert |
| `pid_is` | `pub fn pid_is(pid: u32, start_time: u64) -> bool` | `session/proc.rs:396` | 0/18/0 |
| `is_running` | `pub fn is_running(proc_ref: &ProcRef) -> bool` | `session/proc.rs:410` | 0/8/0 |
| `start_time` | `pub fn start_time(pid: u32) -> Option<u64>` | `session/proc.rs:~420` | 0 |
| `terminate` | `pub fn terminate(pid: u32, start_time: u64) -> io::Result<()>` | `session/proc.rs:~430` | 0 |
| `kill_hard` | `pub fn kill_hard(pid: u32, start_time: u64) -> io::Result<()>` | `session/proc.rs:449` | 0/3/0 |
| `exists` | `pub fn exists(pid: u32) -> bool` | `session/proc.rs:~495` | 0 |
| `Backend` | `pub enum Backend { … }` | `session/xserver.rs:~60` | 0 |
| `Backend::parse/binary/label` | `pub fn …` | `session/xserver.rs:~72-90` | 0 |
| `Display` | `pub struct Display(pub u32);` | `session/xserver.rs:98` | 0 |
| `Display::parse` | `pub fn parse(s: &str) -> Option<Self>` | `session/xserver.rs:106` | 0 |
| `display_is_free` | `pub fn display_is_free(display: Display) -> bool` | `session/xserver.rs:133` | 0 |
| `DisplayClaim` | `pub struct DisplayClaim { … }` | `session/xserver.rs:209` | 0 |
| `DisplayClaim::try_acquire` | `pub fn try_acquire(display: Display) -> io::Result<Option<Self>>` | `session/xserver.rs:216` | 0 |
| `DisplayClaim::Drop` | flock release via rustix | `session/xserver.rs:271` | — |
| `lock_names` | `pub fn lock_names(display: Display, pid: u32) -> bool` | `session/xserver.rs:282` | 0/7/0 |
| `allocate_display` | `pub fn allocate_display(from: u32) -> io::Result<Display>` | `session/xserver.rs:298` | 0/7/0 |
| `claim_display` | `pub fn claim_display(from: Display) -> io::Result<(Display, DisplayClaim)>` | `session/xserver.rs:322` | 0/3/0 |
| `is_listening` | `pub fn is_listening(display: Display) -> bool` | `session/xserver.rs:347` | 0/8/0 |
| `wait_ready` | `pub fn wait_ready(…) -> Result<(), WaitError>` | `session/xserver.rs:370` | 0/7/0 |
| `WaitError` | `pub enum WaitError { … }` | `session/xserver.rs:392` | 0 |
| `XServer` | `pub struct XServer { display, backend, proc, xauth_path }` | `session/xserver.rs:412` | 0 |
| `XServer::is_running` / `stop` | `pub fn …` | `session/xserver.rs:646/657` | 0 |
| `XServerSpec` | `pub struct XServerSpec { … 9 pub fields }` | `session/xserver.rs:426` | 0 |
| `generate_cookie` | `pub fn generate_cookie() -> io::Result<String>` | `session/xserver.rs:451` | 0/5/0 |
| `write_xauth` | `pub fn write_xauth(path, display, cookie) -> io::Result<()>` | `session/xserver.rs:480` | 0/7/0 |
| `open_private_log` | `pub fn open_private_log(path: &Path) -> io::Result<File>` | `session/xserver.rs:518` | 0/4/0 |
| `spawn` | `pub fn spawn(spec: &XServerSpec) -> io::Result<XServer>` | `session/xserver.rs:589` | 0 |

### 3.2 The authoritative external surface (27 items)

Derived from every `maverick_sys::` qualified path outside the crate plus every method invoked on a `ControlHub`/`ControlServer`-typed value. `rg -oN 'maverick_sys(?:::[A-Za-z_]\w*)+'` over `src/ tests/ maverick-*/src/` returns **41 qualified paths across 7 files**; the `use`-alias form (`use maverick_sys::ControlHub;` at `src/backend/x11/hubevents.rs:17`, `use maverick_sys::ControlServer;` at `src/backend/x11/teardown.rs:26`) was checked separately and adds no new symbols.

| # | Symbol | file:line | Real caller(s) |
|---|---|---|---|
| 1 | `DEFAULT_NAME` | `identity.rs:37` | `src/main.rs:97` |
| 2 | `self_info` | `identity.rs:532` | `src/main.rs:291` |
| 3 | `self_info_with_sid` | `identity.rs:553` | `src/main.rs:284` |
| 4 | `InstanceInfo` | `identity.rs:54` | `src/backend/x11/teardown.rs:218` (test) |
| 5 | `write_meta` | `identity.rs:408` | `src/main.rs:319`; `teardown.rs:230` (test) |
| 6 | `cleanup_meta` | `identity.rs:435` | `src/main.rs:413`; `teardown.rs:163` |
| 7 | `meta_path` | `identity.rs:248` | `teardown.rs:231` (test) |
| 8 | `try_sock_path` | `identity.rs:235` | `teardown.rs:250` (test) |
| 9 | `detach_from_terminal` | `lib.rs:431` | `src/main.rs:300` |
| 10 | `Signal` (+`new`,`ignore`,`on_sigterm`,`on_sigcont`,`install`) | `lib.rs:238-350` | `src/main.rs:301,303,305`; `tests/child_lifecycle.rs:34` |
| 11 | `quit_requested` | `lib.rs:214` | `src/backend/x11/mod.rs:677` |
| 12 | `clear_quit` | `lib.rs:220` | `src/backend/x11/mod.rs:678` |
| 13 | `need_regrab` | `lib.rs:226` | `src/backend/x11/mod.rs:673` |
| 14 | `clear_regrab` | `lib.rs:232` | `src/backend/x11/mod.rs:674` |
| 15 | `wait_readable_fds` | `lib.rs:466` | `src/backend/x11/mod.rs:993` |
| 16 | `ControlServer` (type) | `control.rs:137` | `mod.rs:284`; `actions.rs:303`; `teardown.rs:26,153` |
| 17 | `ControlServer::spawn` | `control.rs:175` | `src/main.rs:327`; `teardown.rs:252` (test) |
| 18 | `ControlHub` (type) | `hub.rs:99` | `mod.rs:305`; `actions.rs:315`; `hubevents.rs:17` |
| 19 | `ControlHub::new` | `hub.rs:126` | `src/main.rs:326`; `teardown.rs:252` (test) |
| 20 | `ControlHub::wake_fd` | `hub.rs:166` | `src/backend/x11/mod.rs:991` |
| 21 | `ControlHub::drain_commands` | `hub.rs:250` | `src/backend/x11/actions.rs:325` |
| 22 | `ControlHub::publish_state` | `hub.rs:262` | `src/backend/x11/actions.rs:533` |
| 23 | `ControlHub::emit` | `hub.rs:276` | `src/backend/x11/hubevents.rs:43,52` |
| 24 | `ControlCommand` (all 5 variants) | `hub.rs:76` | `src/backend/x11/actions.rs:330,331,332,333,340` |
| 25 | `control::identity_json` | `control.rs:687` | `src/main.rs:322` |
| 26 | `json::json_escape` | `json.rs:20` | `src/core/ipc.rs:30,56,62,82,106,260,297,303,309,458,464` (11 sites) |
| 27 | `json::{Json, parse, get, str_field, num_field, bool_field, as_array}` | `json.rs:347-473` | `src/core/ipc.rs:507,508,548-566,583-585,602-604,621-625` — **all in `#[cfg(test)] mod tests` (`ipc.rs:499`)** |

**Nothing else in the workspace references `maverick-sys`.** `maverick-core`, `maverick-toml`, `maverick-render`, `maverick-img`, `maverick-vk` do not depend on it at all; `maverick-gl` and `maverick-img` mention it only in prose (`maverick-gl/src/lib.rs:21`, `maverick-img/src/lib.rs:1050`). `Cargo.lock:181-188` confirms `maverick-sys` has exactly one reverse dependency: `maverick`.

---

## 4. Concern decomposition

### (a) Thin OS/FFI primitives — **justified, keep**

| Item | file:line | Needs |
|---|---|---|
| `install_raw` (private) | `lib.rs:396-417` | `libc::sigaction` — **irreducible** |
| `Signal` builder + trampolines | `lib.rs:238-378` | `libc` |
| `quit_requested`/`clear_quit`/`need_regrab`/`clear_regrab` | `lib.rs:103-104, 214-234` | `std::sync::atomic` |
| `detach_from_terminal` | `lib.rs:431-454` | `libc::isatty/open/dup2/close` |
| `wait_readable_fds` | `lib.rs:466-505` | `rustix::event::poll` |
| `peer_uid` | `control.rs:105-117` | `rustix::net::sockopt::socket_peercred` |
| `current_uid`/`current_gid` | `identity.rs:131-145` | `rustix::process::getuid/getgid` |
| `exists` | `session/proc.rs:495-501` | `rustix::process::test_kill_process` |
| `send` (private) | `session/proc.rs:479-489` | `libc::kill` (kernel errno contract) |
| `DisplayClaim` flock + `Drop` | `session/xserver.rs:216-273` | `rustix::fs::open/flock` |
| `generate_cookie` | `session/xserver.rs:451-465` | `rustix::rand::getrandom` |
| `kill_process_group` | `session/lifecycle.rs:712-716` | `rustix::process` |
| `new_session_id` | `identity.rs:269-291` | `rustix::rand::getrandom` |

**Linux-specific:** `/proc` readers (`identity.rs:322-405`, `session/proc.rs`), `rustix`, `std::os::unix`, `UnixListener` — yes, all of it. **Compositor-only:** none of it; the FFI is used by the WM on the non-compositor path too.
**Alternative available elsewhere?** `std` has no `sigaction`, no `poll`, no `SO_PEERCRED`, no `getrandom`. `x11rb` exposes none of these. `libc` covers all of it raw; `rustix` is a safer skin over the same syscalls. **No `std`/`rustix`/`libc`/`x11rb` primitive makes these wrappers unnecessary.**

### (b) Domain logic — **not an OS boundary**

`session` (4,239 prod lines), `ctl` (3,144), `discover` (180), `hub` (307), `json` (668), most of `control` (703) and `identity` (599). Evidence it is not OS boundary:

- `src/json.rs:1-15` — "Pure `&str` → `String` helpers; no global state, no dependencies." A hand-rolled JSON escaper is not a syscall.
- `src/hub.rs:16-17` — "Everything here is plain safe `std`: `Arc`, `Mutex`, and `mpsc`. No `unsafe`, no extra dependencies."
- `src/ctl/mod.rs:1-11` — "the `maverickctl` CLI engine … discover running instances, query their state, run structured queries, send actions, stream events, and quit them … The WM stays minimal; the policy lives here." This is a CLI application.
- `src/session/mod.rs:1-13` — the "Maverick Session" is a product concept: X server + WM + applications, with a lifecycle, logs and a record file. `maverickctl session create` is user-facing CLI surface documented in `README.md` (29 `maverickctl` mentions) and `docs/sessions.md` (46).
- `src/session/lifecycle.rs:1-35` — "Nothing in Maverick supervises that graph — there is no per-session daemon." This is policy: ordering of teardown, reaping, and read commands not mutating.

**This is application logic that happens to be written against a Unix filesystem.** It belongs in a crate named for what it does, not in one named `os::unix-apis`.

### (c) Tests — 1,599 lines in `tests/` + 4,160 lines inline = 5,759 (39% of the crate)

| File | Lines | Covers |
|---|---:|---|
| `tests/signal_install.rs` | 287 | (a) `Signal::install`, real `sigaction` |
| `tests/identity_props.rs` | 292 | (b) sid validation, ficha codec |
| `tests/hub_props.rs` | 288 | (b) hub back-pressure |
| `tests/ctl_props.rs` | 204 | (b) `main_with_args` argv→exit-code |
| `tests/ctl_replies.rs` | 165 | (b) reply classification |
| `tests/control_props.rs` | 146 | (b) `send_command` injection guard |
| `tests/json_props.rs` | 124 | (b) JSON round-trip |
| `tests/common/mod.rs` | 93 | shared strategies |
| inline `#[cfg(test)]` across 12 files | 4,160 | mixed |

**Only 1 of 8 integration test files (`signal_install.rs`, 287 lines, 3.6%) tests an OS primitive.** The other 1,312 lines test application logic.

---

## 5. Zero-caller APIs (deletion candidates)

### 5.1 Zero callers anywhere in the workspace (3)

Exhaustive search: `rg -w <name>` over the entire repo excluding `target/` and `.git/`, plus in-`maverick-sys` internal and test scans.

| Symbol | file:line | Evidence |
|---|---|---|
| `ctl::dispatch_to` | `ctl/mod.rs:474` | `rg -w dispatch_to` → 1 hit (the definition). The doc says "for a subcommand that has already resolved its target"; every subcommand calls `control::dispatch` directly instead (`ctl/windows.rs:537,589,632,649`; `ctl/session.rs`). |
| `discover::find_by_display` | `discover.rs:121` | `rg -w find_by_display` → 1 hit (the definition). The ctl instance-selection uses `find_by_name` (`ctl/mod.rs:613`) and DISPLAY comes from `current_display()` (`ctl/mod.rs:35`). |
| `Session::xserver_is_up` | `session/mod.rs:365` | `rg -w xserver_is_up` → 1 hit (the definition). `derived_state` (`session/mod.rs:373`) only consults `wm_is_up`. |

### 5.2 Public but with no caller outside the crate (137) — grouped

These are the CLI's own API (`ctl`, `control`'s client half, `discover`, `session`) plus pure helpers. They are *not* dead — they are the `maverickctl` binary's surface — but they are **not a library's**: the only consumer is a 9-line `main` in `src/bin/maverickctl.rs`.

- **`control` client half (11 fns):** `send_command`, `ping`, `identify`, `quit`, `restart`, `reload`, `state`, `dispatch`, `query`, `subscribe_stream`, plus `ControlServer::shutdown` and `owner_uid`. Consumers: `ctl/` only.
- **`discover` (7 fns):** `list_instances`, `find_by_name`, `quit_by_name`, `quit_all`, `prune_stale` are called from `ctl/mod.rs:613,627,665,825,851,914` and `session/mod.rs:840,943,987`; `find_by_display` and `ensure_runtime_dir` are dead.
- **`session` (60 items):** consumers are `ctl/session.rs`, `ctl/windows.rs`, `session/lifecycle.rs`, `session/proc.rs`, `session/xserver.rs`. `pub use session::{Session, SessionName, SessionState}` at `lib.rs:518` re-exports three types that **no external crate ever names** — a re-export with no consumer.
- **`json` (20 of 23 items):** `json_quote` (51 internal uses), `scan_object` (`identity.rs:505`, `session/mod.rs:67`), `quote_array`, `json_unescape`, `Json::to_json`, `as_u64`, `as_str_array`, `Field::*` are crate-internal. Only `json_escape` (11 sites) and the `Json` read accessors (test-only, `src/core/ipc.rs:499+`) leave the crate.
- **`identity` (9 cmd-name consts):** `QUIT_CMD`, `PING_CMD`, `IDENTIFY_CMD`, `STATE_CMD`, `RESTART_CMD`, `RELOAD_CMD`, `SUBSCRIBE_CMD`, `DISPATCH_CMD`, `QUERY_CMD` — all `pub`, all used only by `control.rs:67-70` and `discover.rs`. They are wire-protocol vocabulary, not an API; they should be private.

### 5.3 Genuinely duplicate wrappers (2)

| Symbol | file:line | Duplicates |
|---|---|---|
| `session::current_uid` | `session/mod.rs:726-728` | `identity::current_uid` — the body is literally `identity::current_uid()` (`session/mod.rs:727`) |
| `session::current_gid` | `session/mod.rs:731-733` | `identity::current_gid` — body is `identity::current_gid()` (`session/mod.rs:732`) |
| `discover::ensure_runtime_dir` | `discover.rs:175-179` | `identity::ensure_runtime_dir` + `set_private_dir`; no caller |

---

## 6. Dependency footprint

### `Cargo.lock` evidence

```
Cargo.lock:339-350   rustix 1.1.4  ← deps: bitflags, errno, libc, linux-raw-sys, windows-sys
Cargo.lock:439-449   x11rb 0.13.2  ← deps: as-raw-xcb-connection, gethostname, libc, RUSTIX, x11rb-protocol
Cargo.lock:181-188   maverick-sys 0.18.3 ← deps: libc, proptest, RUSTIX, tempfile
Cargo.lock:130-144   maverick 0.18.4 ← deps: …, maverick-sys, …, x11rb
```

`x11rb-0.13.2/Cargo.toml:252-260` declares `rustix = { version = "1.0", default-features = false, features = ["std","event","fs","net","system"] }` — **non-optional**.

**Consequences:**

1. **`rustix` adds no package to the shipped `maverick` binary.** It is already linked via `x11rb`. `maverick-sys` is the only reason the `process` and `rand` features are enabled (`maverick-sys/Cargo.toml:28-37`; rustix-1.1.4 `Cargo.toml:119` `process = ["linux-raw-sys/prctl"]`, `:121` `rand = []` — both feature-only, neither adds a package). The manifest's own comment ("This adds no package: `x11rb` already depends on rustix, and the lockfile already pins it") is **correct**.
2. **But the crate-level conclusion is weaker than it looks.** `rustix` is *only* reachable through `maverick-sys` for the `process`/`rand` features. If the crate is dissolved and the FFI moves into the root binary, `rustix` remains a transitive dep via `x11rb`; if the FFI is re-expressed in `libc` (see §7), `maverick-sys`'s `rustix` request disappears entirely and the *feature set* shrinks even though the package remains.
3. **`libc` is a root dependency too** (`Cargo.toml:48`), used directly at `src/main.rs:301,303,305` for `SIGPIPE`/`SIGCONT` and elsewhere. So `libc` is not removable either way.

**What each buys:**

| Dep | Bought | Verdict |
|---|---|---|
| `libc` | `sigaction` (+`SA_NOCLDWAIT`/`SA_RESTART`), `isatty`/`open`/`dup2`, `kill` with kernel errno, signal-number constants in the public `Signal` API (`libc::c_int`) | **Irreducible** — verified |
| `rustix::event` | `poll` over `BorrowedFd` without hand-rolled `pollfd` layout (`lib.rs:481-500`) | Real, small |
| `rustix::net` | `socket_peercred` with a `NonZeroI32` `Pid` making the uninitialised-buffer bug unrepresentable (`control.rs:105-117`) | Real, small |
| `rustix::process` | `getuid`/`getgid` (`identity.rs:135,144`), `test_kill_process` (`proc.rs:500`), `kill_process_group` (`lifecycle.rs:716`) | Small; `libc::getuid/getgid` are equally safe calls |
| `rustix::fs` | `flock` + `O_NOFOLLOW` display-lock (`xserver.rs:217-271`) | Real; `std` has no `flock` |
| `rustix::rand` | `getrandom` for session-id and Xauthority cookies (`identity.rs:286`, `xserver.rs:456`) | Small; `/dev/urandom` read would work |
| `x11rb` | X11 protocol, Xlib bootstrap, and **the reason rustix is in the graph** | Unrelated to this audit |
| `std` | `UnixStream`/`UnixListener`, `mpsc`, `Arc`/`Mutex`, `atomic`, `fs` | Used throughout |

---

## 7. Per-API recommendation

### 7.1 The OS boundary (KEEP / one REPLACE)

| Symbol | file:line | Verdict | Justification |
|---|---|---|---|
| `Signal` + `install`/`new`/`ignore`/`on_sigterm`/`on_sigcont` | `lib.rs:238-350` | **KEEP** | 2 real consumers (`src/main.rs:301-305`, `tests/child_lifecycle.rs:34`); owns a real invariant — `SIGCHLD` always installed with `SA_NOCLDWAIT\|SA_RESTART` (`lib.rs:326-332`), and a refused disposition is **reported**, not swallowed (`lib.rs:396-416`). rustix cannot replace it (verified, §10). |
| `install_raw` (private) | `lib.rs:396-417` | **KEEP** | Single disposition primitive; the `sigaction` layout, the handler-union member and the `sa_mask` are argued once (`lib.rs:380-391`). |
| `quit_requested`/`clear_quit`/`need_regrab`/`clear_regrab` | `lib.rs:214-234` | **KEEP** | 4 consumers, one each (`src/backend/x11/mod.rs:673,674,677,678`); owns the "handler is a notification, not a control path" invariant (`lib.rs:63-79`) and deliberately has **no public setter** for the quit flag. |
| `detach_from_terminal` | `lib.rs:431-454` | **KEEP** | 1 consumer (`src/main.rs:300`); owns a non-obvious invariant — it deliberately does **not** call `setsid` (`lib.rs:423-430`, Xorg DRM-master loss). `std` has no equivalent. |
| `wait_readable_fds` | `lib.rs:466-505` | **KEEP** | 1 consumer (`src/backend/x11/mod.rs:993`); owns the `tv_nsec`-is-a-remainder split (`lib.rs:485-494`) and the EINTR-as-wakeup rule (`lib.rs:501-504`), both of which callers depend on. Already on `rustix::event::poll` — no swap needed. |
| `peer_uid` | `control.rs:105-117` | **MOVE** (to the FFI module) | 0 external callers; 1 internal (`control.rs` handler). Irreducible primitive, but it is not a control-protocol function and does not belong in `control.rs`. |
| `identity::current_uid`/`current_gid` | `identity.rs:131-145` | **KEEP** | The kernel is the only acceptable source of ownership (`identity.rs:130`, `control.rs:176-179`); two-line infallible reads, no `SAFETY` note needed. |
| `session::current_uid`/`current_gid` | `session/mod.rs:726-733` | **DELETE** | Bodies are literally `identity::current_uid()` / `identity::current_gid()` — exact duplicates with zero external callers. |
| `session::proc::send` (private) | `session/proc.rs:479-489` | **KEEP** | `libc::kill` is a deliberate choice so a refused signal reports the **kernel's** errno; `rustix::process::Signal` is a `NonZeroI32` newtype and would reject `0x7FFF` locally (`proc.rs:470-478`). This is a real, tested contract. |
| `proc::exists` | `session/proc.rs:495-501` | **KEEP** | Uses `test_kill_process`; the `Pid(NonZeroI32)` type makes pid 0 unrepresentable (`proc.rs:493-501`), a real safety property. |
| `DisplayClaim` (`try_acquire`, `Drop`) | `session/xserver.rs:216-273` | **KEEP** | Owns the `flock` display-allocation invariant with `LOOP`→retry and `AGAIN`→try-else (`xserver.rs:249-261`). `std` has no `flock`. |
| `generate_cookie`, `new_session_id` | `xserver.rs:451`, `identity.rs:269` | **REPLACE → `std`** (optional) | Both are `rustix::rand::getrandom` for 16/32 random bytes. `std` has no CSPRNG; reading `/dev/urandom` would be a downgrade. **Keep rustix** — but note these are the *only* reason the `rand` feature is requested. |
| `kill_process_group` use | `lifecycle.rs:712-716` | **KEEP** | Uses rustix `Pid`/`Signal` types that make pgid 0 unrepresentable. |

### 7.2 The control plane (INLINE / KEEP with a caveat)

| Symbol | file:line | Verdict | Justification |
|---|---|---|---|
| `ControlHub` (type) | `hub.rs:99` | **KEEP** | 3 consumer files; owns two non-obvious invariants: (1) the self-pipe is drained **before** the command queue, or a queued command is lost (`hub.rs:236-249`); (2) every queue is non-blocking with a stated full policy (`hub.rs:30-47`). Pure `std`, no FFI, but a real abstraction. |
| `ControlHub::new`/`wake_fd`/`drain_commands`/`publish_state`/`emit` | `hub.rs:126/166/250/262/276` | **KEEP** | Real consumers, listed in §3.2 rows 19-23. |
| `ControlHub::snapshot` | `hub.rs:189` | **KEEP** | 0 external, 1+ internal: `control.rs` answers `state` from it. Not dead. |
| `ControlHub::subscribe` | `hub.rs:205` | **INLINE → private** | 0 external, 0 internal outside its own tests; `control.rs` uses `try_subscribe` (the capped variant) exclusively. Two registration paths where one is used is a footgun. Make it private or delete. |
| `ControlHub::push_command` | `hub.rs:153` | **KEEP** | Called by `control.rs` on every dispatch/quit/restart/reload. |
| `ControlHub::try_subscribe` | `hub.rs:223` | **KEEP** | Owns the atomic cap-and-register under one critical section (`hub.rs:216-231`). |
| `ControlHub::subscriber_count`/`dropped_events` | `hub.rs:291/297` | **KEEP (tests-only)** | Zero external, used only by `hub.rs:312-420` and `tests/hub_props.rs`. They are observability, documented as such (`hub.rs:290`, `hub.rs:295`). Acceptable, but they should arguably be `#[cfg(any(test, feature = "..."))]`. |
| `CMD_CAP`/`SUB_CAP`/`MAX_SUBSCRIBERS`/`MAX_LINE_LEN`/`MAX_CMD_LEN` | `hub.rs:61,67`; `control.rs:86,89,91` | **MAKE PRIVATE** | Wire/capacity vocabulary, not API. `MAX_CMD_LEN` is used by `tests/control_props.rs:12` — a `pub(crate)` + a test-only re-export, or leave as-is; the other four have no external consumer. |
| `ControlCommand` | `hub.rs:76` | **KEEP** | 5 variant matches at `src/backend/x11/actions.rs:330-340`; deliberately not `Eq` because `Query` carries a `SyncSender` (`hub.rs:72-74`). |
| `ControlServer` (type) + `spawn` | `control.rs:137,175` | **KEEP** | 3 consumer files. Owns real invariants: uid read from the kernel not passed in (`control.rs:176-179`), `0700` dir + `0600` socket, TOCTOU-hardened stale-socket unlink via `symlink_metadata` + `is_socket` (`control.rs:190-205`), and `Drop` unlinks the socket. |
| `ControlServer::shutdown` | `control.rs:282` | **INLINE → `pub(crate)`** | 0 external callers; `Drop` already calls it (`control.rs:41-43` module docs). Public but unreachable from outside. |
| `ControlServer::owner_uid` | `control.rs:150` | **MAKE PRIVATE / test-only** | Doc says "Exposed so the behaviour can be *checked* rather than only claimed" (`control.rs:146-149`) — i.e. it exists for the test at `tests/signal_install.rs`-style fixtures. 0 production consumers. |
| `send_command` + `ping`/`identify`/`quit`/`restart`/`reload`/`state`/`dispatch`/`query` | `control.rs:542-647` | **MOVE (as a set)** | Zero external callers; consumers are `ctl/mod.rs`, `ctl/windows.rs`, `ctl/session.rs`, `discover.rs`, `session/mod.rs:359`. This is the **client half of a protocol that lives in the same crate** — it belongs next to the CLI that speaks it, not behind the crate's front door. |
| `subscribe_stream` | `control.rs:649` | **KEEP (CLI-only)** | Consumers: `ctl/mod.rs` `subscribe` verb. Move with the client half. |
| `identity_json` | `control.rs:687` | **MOVE** | 1 external consumer (`src/main.rs:322`). It serialises `InstanceInfo` — it is a `identity` function wearing a `control` name. |
| `identity::*_CMD` (9 consts) | `identity.rs:40-50` | **MAKE PRIVATE** | Wire vocabulary, used only by `control.rs:67-70` and `discover.rs`. |
| `identity::runtime_dir`/`session_dir`/`sock_path`/`try_*`/`meta_path`/`is_valid_sid`/`MAX_SID_LEN`/`read_meta`/`new_session_id`/`current_display`/`current_tty_nr`/`read_proc_*` | `identity.rs:104-405, 595` | **MOVE (as a set)** | Zero external callers. `write_meta`/`cleanup_meta`/`meta_path`/`try_sock_path`/`self_info*`/`InstanceInfo` **do** have external callers and must stay reachable. |
| `InstanceInfo` + 11 pub fields | `identity.rs:54` | **KEEP (type), narrow fields** | 11 pub fields, but external code only constructs it once (`src/backend/x11/teardown.rs:218-229`, a test) and never reads a field. All 11 can be `pub(crate)` without breaking anything outside. |
| `discover::*` | `discover.rs:41-179` | **MOVE (whole module)** | 5 of 7 are called only by `ctl/mod.rs` and `session/mod.rs`; 2 are dead. |
| `discover::find_by_display` | `discover.rs:121` | **DELETE** | 0 callers, anywhere. |
| `discover::ensure_runtime_dir` | `discover.rs:175` | **DELETE** | 0 callers; `identity::ensure_runtime_dir` (`identity.rs:160`) is what `control.rs:184` and `session/mod.rs:702` call. |
| `ctl::*` (37 items) | `ctl/` | **MOVE (whole module)** | 0 external callers. The only entry point is `main_with_args` (`ctl/mod.rs:80`), called from `src/bin/maverickctl.rs:8`. |
| `ctl::dispatch_to` | `ctl/mod.rs:474` | **DELETE** | 0 callers, anywhere. |
| `session::*` (60 items) | `session/` | **MOVE (whole module tree)** | 0 external callers. Consumers are `ctl/` and each other. |
| `Session::xserver_is_up` | `session/mod.rs:365` | **DELETE** | 0 callers, anywhere. |
| `session::marked_pids` | `session/proc.rs:365` | **DELETE or make private** | Only its own test (`proc.rs:790`) and 2 doc mentions call it. The module docs at `proc.rs:27-29` say callers "that need the widest possible net can sweep for it" — no caller does. |
| `json::json_escape` | `json.rs:20` | **KEEP** | 11 real external call sites (`src/core/ipc.rs`). It is a pure function, not an OS primitive — so it is the one `json` item that genuinely earns a cross-crate edge. |
| `json::{json_quote, json_unescape, quote_array, scan_object, Value, Field, Field::*, Json, Json::*}` | `json.rs:39-473` | **KEEP (crate-internal)** | 0 or test-only external callers. `Json`/`parse`/`get`/`str_field`/`num_field`/`bool_field`/`as_array` are used only inside `src/core/ipc.rs`'s `#[cfg(test)] mod tests` (`ipc.rs:499`) — that is a **test-only** cross-crate edge and is a smell: production `ipc.rs` writes JSON with `json_escape` and never parses it back; only the tests do. |
| `lib.rs` re-exports `Session`, `SessionName`, `SessionState` | `lib.rs:518` | **DELETE the re-export** | `rg -w 'SessionName\|SessionState\|maverick_sys::Session'` over `src/ tests/ maverick-*/` → 0 hits. A re-export with no consumer. |

### Verdict counts

| Verdict | Count |
|---|---:|
| **KEEP** | 30 |
| **MOVE** (module or coherent set) | 9 items / ~152 symbols |
| **INLINE** (make private / fold into caller) | 4 |
| **REPLACE** | 1 (optional `rand`→`/dev/urandom`, recommended against) |
| **DELETE** | 6 named + 2 re-exports + 9 consts demoted to private |

---

## 8. Proposal: target shape of the OS boundary

**The crate should not shrink. It should split — into one crate that is genuinely an OS boundary, and one binary crate that is genuinely an application.**

### 8.1 What `maverick-sys` becomes (≈250 production lines)

Keep: `lib.rs` (`Signal`, flags, `detach_from_terminal`, `wait_readable_fds`), `peer_uid`, `current_uid`/`current_gid`, the `proc` signal helpers, the `DisplayClaim` flock helper, `generate_cookie`/`new_session_id`.

`categories = ["os::unix-apis"]` becomes true. Public surface drops from 277 items to roughly 35. `maverick-sys/src/lib.rs` is the *only* file in the workspace that mentions `libc::sigaction`.

### 8.2 What leaves

- **`session/` (4,239 lines) + `ctl/` (3,144) + `discover/` (180) + `json/` (668) + the client half of `control/` + the non-`write_meta`/`cleanup_meta` parts of `identity/`** → a new crate, e.g. `maverick-ctl` or `maverick-session`, holding `src/bin/maverickctl.rs` and the `ctl` engine. This crate is the *only* consumer of all of it, so the move is mechanical.
- **`json_escape`** is the single exception that must stay reachable from the WM: either keep `json` in `maverick-sys` (it is 668 pure lines, and one of them has an external consumer), or move it to `maverick-core`, which already has **zero dependencies** (`maverick-core/Cargo.toml` `[dependencies]` is empty) and is the crate the campaign's other audits treat as the pure-logic home. `json_escape` needs nothing, so `maverick-core` is the cleaner home — but it adds an edge to `maverick-core` that does not exist today.

### 8.3 Exact `Cargo.toml` consequences (**not applied**)

```toml
# maverick-sys/Cargo.toml — after the split
[package]
description = "Maverick's OS/FFI boundary: signal dispositions, poll, uid/gid,
               peer credentials, process-tree signalling, X display locking"
categories = ["os::unix-apis"]            # unchanged, and now true
[dependencies]
libc  = "0.2"                             # UNCHANGED — sigaction, SA_NOCLDWAIT, SA_RESTART
rustix = { version = "1", default-features = false, features = [
    "std", "event", "fs", "net", "process", "rand",   # UNCHANGED — all five still live
] }
[[test]]                                   # DELETED — ctl_props belongs to the new crate
name = "ctl_props"
path = "tests/ctl_props.rs"
[dev-dependencies]
proptest  = { workspace = true }           # DELETED — only the ctl/session/json/identity props need it
tempfile  = { workspace = true }           # DELETED — signal_install.rs does not use it
# tests/{ctl_props,ctl_replies,control_props,hub_props,identity_props,json_props}.rs
# tests/common/mod.rs  → moved with the crate

# NEW maverick-ctl/Cargo.toml
[package]
name = "maverick-ctl"
[dependencies]
maverick-sys = { path = "../maverick-sys" }   # for identity::write_meta/cleanup_meta on the socket path
[[bin]]
name = "maverickctl"
path = "src/main.rs"
[dev-dependencies]
proptest.workspace = true
tempfile.workspace = true

# root Cargo.toml
[workspace] members += [ "maverick-ctl" ]
maverick-sys = { path = "maverick-sys" }      # UNCHANGED — still mandatory
maverick-ctl = { path = "maverick-ctl", optional = true }   # if gated; see below
```

**`install.sh` must change.** `install.sh:1478/1483` builds `cargo build --release -p maverick -p maverick-sys`. After the split that becomes `-p maverick -p maverick-ctl`, and `install.sh:150` `RUNTIME_BINS=(maverick maverickctl)` stays correct. `install.sh:1757-1778` (`verify_installed "$BIN_DIR/maverickctl session --help"`) is unaffected. **`tests/install-smoke.py:129-131,161-164,443-475` also asserts the binary set** and will need its `-p` list updated.

**Alternative if the campaign wants `maverick-sys` gone entirely:** the ~250 FFI lines move into a `src/sys/` module of the root `maverick` crate (matching the existing `maverick-gl` precedent: *"no binding-generator `extern "C"`, in the same spirit as `maverick-sys`"* — `maverick-gl/src/lib.rs:21`), and the CLI becomes `maverick-ctl`. That removes the `maverick-sys` package from the graph but **does not remove `rustix`** (still via `x11rb`, `Cargo.lock:447`) **and does not remove `libc`** (root `Cargo.toml:48`). The win is ~250 lines of honest naming, not a dependency.

---

## 9. Evidence appendix

### A. rustix `sigaction` — the manifest's claim, VERIFIED

```
~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/rustix-1.1.4/src/not_implemented.rs:66-72
  pub mod libc_internals {
      not_implemented!(exit);
      not_implemented!(fork);
      not_implemented!(clone);
      not_implemented!(clone3);
      not_implemented!(brk);
      not_implemented!(sigaction);      ← line 72
```

*Nuance worth recording:* rustix **does** expose `rustix::runtime::kernel_sigaction` (`rustix-1.1.4/src/runtime.rs:503`) plus `KernelSigactionFlags` with `NOCLDWAIT` (`runtime.rs:98`) and `RESTART` (`runtime.rs:110`). But `runtime.rs:41-46` says: *"This module is intended to be used for implementing a runtime library such as libc. Use of these features for any other purpose is likely to create serious problems."* The manifest's claim stands for the supported API surface, and the `runtime` module is not a substitute.

### B. `unsafe` count and confinement

**Production `unsafe` blocks in `maverick-sys/src`: 4.**

| Site | What |
|---|---|
| `src/lib.rs:405-415` | `libc::sigaction` in `install_raw` — the crate's only disposition primitive |
| `src/lib.rs:432-453` | `libc::isatty`/`open`/`dup2`/`close` in `detach_from_terminal` |
| `src/lib.rs:479` | `std::os::unix::io::BorrowedFd::borrow_raw` in `wait_readable_fds` |
| `src/session/proc.rs:485` | `libc::kill` in `send` |

Test-only `unsafe`: `src/lib.rs:164,177,184`; `tests/signal_install.rs:109,168`. All other `unsafe` hits in `maverick-sys/src` are prose in doc comments (`hub.rs:17`, `identity.rs:132`, `lib.rs:9,10,12,92,462,464`).

**`unsafe` is fully confined.** Every production block carries a `// SAFETY:` note except `lib.rs:479`, which has one at `lib.rs:477-478`. The claim at `lib.rs:81-95` is accurate for this crate (and correctly says it is *not* confined to this crate — `maverick-gl`, `maverick-vk`, `maverick-x11` and the root X11 backend have their own).

### C. `maverickctl` is a shipped binary of this crate

```
maverick-sys/src/bin/maverickctl.rs            (9 lines)  — no [[bin]] in maverick-sys/Cargo.toml;
                                                             auto-discovered by Cargo
install.sh:150    RUNTIME_BINS=(maverick maverickctl)
install.sh:1478   cargo build --release $CARGO_FEATURES -p maverick -p maverick-sys
install.sh:1483   cargo build --release $CARGO_FEATURES -p maverick -p maverick-sys
install.sh:1775-1778  verify_installed "$BIN_DIR/maverickctl --help" / "session --help"
README.md:29 maverickctl mentions · docs/sessions.md:46
```

### D. Method: how the caller table was built

1. Extracted every `pub`/`pub(crate)` declaration from `maverick-sys/src/**/*.rs` (277 items) with file:line.
2. For each, `rg -n -w <name>` over the external set (`src/ tests/ maverick-{core,x11,gl,toml,render,img,vk}/src`) and separately over `maverick-sys/src` (excluding the definition line) and `maverick-sys/tests`.
3. Independently extracted every `maverick_sys::…` qualified path outside the crate (`rg -oN 'maverick_sys(?:::\w+)+'`) — **41 paths across 7 files** — and checked `use maverick_sys::…` aliases (`hubevents.rs:17`, `teardown.rs:26`).
4. For `ControlHub`/`ControlServer` methods (which appear unqualified at call sites), located every file binding those types and grepped for `.<method>(` there, excluding `engine.bus`/`bus` (a different type, `core/engine.rs:73`).
5. Repo-wide sweep for the deletion candidates: `rg -w <name>` over the whole tree excluding `target/` and `.git/`, so shell scripts, Python harnesses, C probes and docs were covered.

### E. The complete list of qualified `maverick_sys::` paths outside the crate

```
src/main.rs:97          maverick_sys::DEFAULT_NAME
src/main.rs:284         maverick_sys::identity::self_info_with_sid
src/main.rs:291         maverick_sys::self_info
src/main.rs:300         maverick_sys::detach_from_terminal
src/main.rs:301,303,305 maverick_sys::Signal::{new,on_sigterm,on_sigcont,install}
src/main.rs:319         maverick_sys::identity::write_meta
src/main.rs:322         maverick_sys::control::identity_json
src/main.rs:326         maverick_sys::ControlHub::new
src/main.rs:327         maverick_sys::ControlServer::spawn
src/main.rs:413         maverick_sys::identity::cleanup_meta
src/backend/x11/mod.rs:284,305     ControlServer, ControlHub (field types)
src/backend/x11/mod.rs:673,674,677,678  need_regrab, clear_regrab, quit_requested, clear_quit
src/backend/x11/mod.rs:991         hub.wake_fd()
src/backend/x11/mod.rs:993         maverick_sys::wait_readable_fds
src/backend/x11/actions.rs:303,315 ControlServer, ControlHub
src/backend/x11/actions.rs:325     h.drain_commands()
src/backend/x11/actions.rs:330-340 ControlCommand::{Quit,Restart,Reload,Dispatch,Query}
src/backend/x11/actions.rs:533     hub.publish_state()
src/backend/x11/hubevents.rs:17,43,52  ControlHub, hub.emit() ×2
src/backend/x11/teardown.rs:26,153  ControlServer
src/backend/x11/teardown.rs:163    identity::cleanup_meta
src/backend/x11/teardown.rs:218,230,231,250,252  (test) InstanceInfo, write_meta, meta_path,
                                                     try_sock_path, ControlServer::spawn, ControlHub::new
src/core/ipc.rs:30,56,62,82,106,260,297,303,309,458,464  json::json_escape ×11
src/core/ipc.rs:507,508,548-566,583-585,602-604,621-625  (test) json::{Json,parse,get,str_field,
                                                                  num_field,bool_field,as_array}
tests/child_lifecycle.rs:34        maverick_sys::Signal::new + install
```

---

## 10. OUT OF SCOPE BUGS / unverified claims

**Not fixed, per instructions.**

1. **`find_by_display` is dead code, not a bug** — but its existence is evidence that DISPLAY-based instance selection was planned and never wired. `ctl/mod.rs:21-23` documents the `--name` path via `find_by_name`; the DISPLAY+TTY path (`ctl/mod.rs` step 4) uses `current_display()` + `current_tty_nr` and filters `list_instances()` directly, never `find_by_display`. Either the doc or the function is redundant.

2. **`discover::ensure_runtime_dir` vs `identity::ensure_runtime_dir` is a live divergence risk** (`discover.rs:175` vs `identity.rs:160`). The `discover` one is dead today; if someone calls it they get `io::Result<()>` and lose the `PathBuf`, silently diverging from the 0700-creation path. Not a live bug; a trap.

3. **`session::current_uid`/`current_gid` are byte-for-byte re-exports** (`session/mod.rs:726-733`) with doc comments that *look* like they justify a decision ("The kernel's answer, via the crate's single credential wrapper") when the body is a one-line forward. Not a bug; documentation that describes a wrapper that does not exist.

4. **JSON round-trip is untested across the crate boundary.** Production `src/core/ipc.rs` writes JSON using `maverick_sys::json::json_escape` (11 sites, `ipc.rs:30-464`). The parsing side (`maverick_sys::json::parse`, `Json`, `get`, `str_field`, `num_field`, `bool_field`, `as_array`) is used **only** inside `ipc.rs`'s own `#[cfg(test)] mod tests` (`ipc.rs:499`). The *actual* production consumer of that parser is `maverickctl` inside `maverick-sys` (`ctl/windows.rs:284,418,421,518,765,809,825`). So the WM's own JSON writer is validated by tests that exercise a different parser path than production does — the WM never parses its own output. `[UNVERIFIED]` whether the writer and the `maverickctl` parser can disagree in a way the tests would not catch; the `json_props.rs` suite (124 lines) covers the codec itself, which partially mitigates this.

**Unverified claims:**

- `[UNVERIFIED]` Whether the `maverick` binary's *link-time* size or binary content changes if `rustix`'s `process` and `rand` features are dropped. Reasoning is from `Cargo.lock` and the manifests; no build was run (out of scope). The package itself is definitely still present (`Cargo.lock:339`), and `process`/`rand` are feature-only (`rustix-1.1.4/Cargo.toml:119,121`).
- `[UNVERIFIED]` Whether `install.sh` and `tests/install-smoke.py` have any *other* build invocation that would need the `-p` list updated beyond lines 1478/1483. Grep for `cargo build` in `install.sh` returned only those two.
- `[UNVERIFIED]` `maverick-vk` is a workspace member (`Cargo.toml:8`) but is not in `Cargo.lock`'s `maverick` dependency list — it is unreferenced. Not this audit's concern.
