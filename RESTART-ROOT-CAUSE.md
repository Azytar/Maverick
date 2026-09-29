# RESTART ROOT CAUSE — `maverickctl restart` is acknowledged before it has happened

Scope: the whole `maverickctl restart` path
(`maverickctl` → unix socket → restart command → teardown → `exec` → new instance → new control connection).
Window-ownership *semantics* belong to Agent F; runtime stress belongs to Agent G. This
document covers the control plane, the process/descriptor/session handoff, and the state the
control path loses.

**Verdict on the prior hypothesis in this file (it is the text being replaced):** the *chain* is
wrong, though one link of it is real. The prior text claims `restart()` re-execs with
`launch_args` only, so the sid churns and a stale `MAVERICK_INSTANCE` makes `resolve_target`
fail, forcing a new terminal. Three of those four steps do not survive contact with the code:

1. The re-exec already preserves the sid. `src/backend/x11/actions.rs:157` calls
   `restart_args(&self.launch_args, &self.session_id)`, and `restart_args`
   (`actions.rs:436-443`) appends `--session-id <current-sid>` when the original argv lacks it.
2. A stale `MAVERICK_INSTANCE` does **not** make `resolve_target` fail. `ctl/mod.rs:604-608`
   only returns the env value when `read_meta` succeeds; when it does not, control *falls
   through* to the DISPLAY+TTY path. The env var degrades targeting, it does not break it.
   Agent G measured this directly (F3/F4 in `RESTART-STRESS.md`).
3. "A fresh terminal has no `MAVERICK_INSTANCE`" is wrong for the common case. A terminal
   emulator opened from a keybind, from the WM's own menu, or via `maverickctl exec` is a
   **descendant of the WM process** and therefore inherits `MAVERICK_INSTANCE` from
   `src/main.rs:297` — the same variable, just a *different value*. The discriminating factor
   is the value, not its absence.

What is left is a defect that is independent of the sid, survives the sid fix, and explains
every symptom in the report: **`restart` is acknowledged at queue-push time, and the instance
is unaddressable from the moment it tears itself down until the replacement binds its socket.**
Between those two instants `maverickctl` cannot reach the WM by *any* means, and the first
`restart` reported success while that was already true.

---

## 1. What state survives restart?

| State | Survives? | Where |
|---|---|---|
| The X server and every client window | **yes** | the WM is re-exec'd in place; no X server is touched |
| The WM's **pid** | **yes** | `Command::exec` replaces the image, it does not fork (`actions.rs:158`) |
| The WM's kernel **start time** (`/proc/<pid>/stat` field 22) | **yes** | `exec` does not change it; so `exec` is invisible to the liveness check |
| The WM's POSIX **session**, **process group** and **controlling terminal** | **yes** | no `setsid` is ever called — `maverick_sys/src/lib.rs:423-430` says so explicitly, `detach_from_terminal` (`lib.rs:431-454`) only `dup2`s `/dev/null` over stdin/stdout, and only when `isatty(STDIN)` |
| `DISPLAY`, `XDG_RUNTIME_DIR`, and the whole environment | **yes** | `Command::exec` inherits the parent's environ; `identity::runtime_dir()` re-reads `XDG_RUNTIME_DIR` on every call (`identity.rs:104-113`) |
| The **session id**, the socket path, the ficha path | **yes** (with the fix in `actions.rs:436-443`) | `--session-id` is re-passed on the re-exec |
| `MAVERICK_INSTANCE` in *already-running* children | **yes, unchanged** | a `set_var` in a child never propagates back; but see §6 |
| The `MAVERICK_INSTANCE` the WM hands to *future* children | **yes, and now correct** | `main.rs:297` re-exports the same sid after exec |
| `stdin`/`stdout` = `/dev/null` | **yes** | installed on the first start; on the re-exec `isatty(0)` is false so `detach_from_terminal` returns early (`lib.rs:435-437`) |
| `stderr` still pointing at the original terminal | **yes** | never redirected (`lib.rs:449`) — WM logs keep landing in the terminal that started the WM, across every restart |

## 2. What state is lost?

| State | Lost at | Consequence |
|---|---|---|
| **The control socket pathname** | `teardown.rs:163` → `identity::cleanup_meta` → `identity.rs:448-463` | `connect()` returns `ENOENT` for every client |
| **The identity ficha** | same call, `identity.rs:439-447` | `read_meta` → `None`; the instance is invisible to `list_instances` and to `resolve_target` |
| **The control listener fd and the accept thread** | `ControlServer::drop` (`control.rs:299-303`) and then `exec` | the listener is `SOCK_CLOEXEC`, so `exec` closes it anyway |
| **Every in-flight control command** | the same `drain_commands` batch, if `Restart` precedes them | `hub.rs:250-259` returns the whole batch; `drain_control` (`actions.rs:256-287`) execs on the `Restart` arm and never reaches the rest |
| **`subscribe` streams** (bars, `maverickctl subscribe`) | accepted-stream fds are `SOCK_CLOEXEC` and die at `exec` | `subscribe_stream` (`control.rs:649-...`) sees a clean EOF and returns `Ok(())` — the client is told the stream ended *normally* and must reconnect on its own. Nothing in the WM re-establishes it. |
| **The cached `identify` JSON** | new process, `main.rs:321` | regenerated |
| **The hub's command queue, state snapshot, subscriber list** | whole process image | regenerated |
| **All in-memory WM state** (grabs, focus, layout, tags, `last_key_times`, `arrange_dirty`, `docks`, `last_stacking_order`, EWMH root properties) | whole process image | `teardown_x` (`mod.rs:561-596`) releases the X-visible half explicitly; the rest is rebuilt by `WindowManager::new` |

## 3. Why are windows detached? (handoff timing only)

The control path and the screen are released at two *different* moments, and the second is
much later than the first.

```
control plane                                     screen
---------                                         ------
ControlCommand::Restart dequeued  actions.rs:259
cleanup()                                         actions.rs:136
  teardown_x():  root event mask = NO_EVENT  mod.rs:574-577   ← SubstructureRedirect RELEASED
                  EWMH root props deleted     mod.rs:583-586
                  check window destroyed      mod.rs:586
                  flush()                     mod.rs:594       ← the X server has now seen it
  run_local():    cleanup_meta(sid)           teardown.rs:163  ← socket + ficha GONE
                  drop(control)               teardown.rs:169
FC_CLOEXEC on the X fd                           actions.rs:146-151
exec()                                           actions.rs:158
----------------- screen is unowned; instance is unaddressable -----------------
new image: ld.so, runtime init, log::init, argv parse          main.rs:91-183
            self_info_with_sid → write_meta                     main.rs:282-320  ← ficha BACK
            ControlServer::spawn → bind                         main.rs:326      ← socket BACK
            load_config                                          main.rs:334
            open_x, Atoms::new, check_no_other_wm                mod.rs:772-800   ← screen RECLAIMED
            setup_root, scan_windows, arrange                   mod.rs:932-950
```

Three distinct gaps, in increasing size:

* **Unaddressable gap** — `teardown.rs:163` → `main.rs:326`. During this window the instance
  does not exist for `maverickctl`: no socket, no ficha. Any command in it fails.
  *Magnitude*: `execve` + dynamic linking + Rust runtime init + `self_info` (four `/proc`
  reads + `getrandom`) + five `sigaction`s + `write_meta` + `bind`. The binary under test is a
  43 MB unstripped debug image (`target/debug/maverick`), so the relink/runtime-init portion
  alone is tens to hundreds of milliseconds. **This is the gap that matters and it is the one
  no amount of waiting for the "right sid" fixes.**
* **Screen-unowned gap** — `mod.rs:594` → `mod.rs:799`. Longer (it additionally spans
  `load_config` and the X connect + atom interning), but harmless for adoption: the new
  instance claims `SubstructureRedirect` at `mod.rs:799` *before* `scan_windows` at
  `mod.rs:933`, so a window mapped inside the gap is picked up by the tree scan
  (`manage.rs:88-119`, which manages every non-`override_redirect` `VIEWABLE` child), and a
  window mapped after the claim produces a real `MapRequest`. The only windows genuinely lost
  are those that map **and** withdraw entirely inside the gap.
* **Ownership gap for `maverickctl`** — the two above are disjoint in time; the unaddressable
  gap is strictly the earlier and the shorter of the two.

**What the control path additionally loses across the handoff:** the readiness information.
Nothing in the protocol tells a client "the old instance is gone and the new one is not here
yet". There is no generation counter, no `--wait-for` on `restart`, no "reconnect" contract
on `subscribe`. The control plane is a set of files on disk with no lease, no epoch and no
handshake, so every client has to poll or guess.

## 4. Why does the control path become unusable after one restart?

Because `restart` is **acknowledged at enqueue time, not at completion time**, and the
acknowledgement is the last thing the client sees.

* Server side: `control.rs:443-449` answers `restart` with `"ok\n"` the instant
  `hub.push_command(ControlCommand::Restart)` succeeds. It has not touched the WM yet.
* Client side: `ctl/mod.rs:872` → `control::restart` → `send_command`
  (`control.rs:542-588`) reads that one line, `cmd_simple` prints
  `maverickctl: '<sid>' restart` (`ctl/mod.rs:889`) and returns `ExitCode::SUCCESS`.
* The WM has, at that point, not even begun `cleanup()`. It is still inside
  `wait_readable_fds` (`lib.rs:466-505`) and will only reach `restart()` on the next
  `drain_control` (`actions.rs:259`).

So `maverickctl restart` returns *success* while the instance is still up, and returns *success*
again long after the instance has become unaddressable. The tool has no notion of "restart
finished". Every caller — a human, a script, a loop, an agent — is told the operation
completed when in fact a teardown is about to begin and a startup is in progress.

Contrast the session-manager path, which gets this right: `lifecycle::start_maverick`
(`session/lifecycle.rs:579-593`) polls `control::ping` **and** `control::query(name,"state")`
inside `wait_until(START_TIMEOUT, …)` before reporting the session started. `maverickctl
restart` has no equivalent. That asymmetry is the whole bug.

The second restart then lands in the unaddressable gap of §3 and fails at
`UnixStream::connect` (`control.rs:559`) with `ENOENT` — printed by `cmd_simple` as
`maverickctl: restart failed: No such file or directory (os error 2)`. It is not a *broken*
restart; it is a restart issued into a window in which no instance exists.

## 5. Is a stale env var the ONLY mechanism? No.

Ranked by confidence, with the evidence for each.

### 5.1 Asynchronous acknowledgement + the unaddressable gap — **confidence: very high (proven by code)**
`control.rs:443-449`, `ctl/mod.rs:872/889`, `teardown.rs:163-169`, `main.rs:318-326`.
The mechanism above. It is the only defect that survives the `--session-id` fix, and it is the
only one that needs no terminal, no env var and no timing coincidence beyond "the next command
arrives quickly". `lifecycle::start_maverick`'s `wait_until` is the proof that the codebase
already knows this is required and simply did not apply it to `restart`.

### 5.2 `resolve_target`'s env branch gates on *existence*, not *liveness* — **confidence: high (proven by code)**
`ctl/mod.rs:604-608`:
```rust
if let Ok(env) = std::env::var("MAVERICK_INSTANCE") {
    if !env.is_empty() && crate::identity::read_meta(&env).is_some() {
        return Some(env);
    }
}
```
`read_meta` only proves a file is parseable. It does **not** ping. So a sid whose ficha is
present but whose socket is not bound is returned immediately, and the subsequent
`control::restart(sid)` fails at `connect` **with no fallback to the context path** — the
function has already returned. This is the one place in `resolve_target` where a bad answer is
not self-correcting.

That state is reachable, and reachable *asymmetrically*, because the two artefacts are created
and destroyed in opposite orders:

| | order | window where they disagree |
|---|---|---|
| teardown | `cleanup_meta` removes **socket then ficha**, in one call (`identity.rs:439-463`) | ~microseconds |
| startup | `write_meta` at `main.rs:318`, then `ControlServer::spawn` at `main.rs:326` | `main.rs:318` → `main.rs:326` |

The startup window is small — but it is not the interesting part. The interesting part is
what happens when `ControlServer::spawn` **fails**: `main.rs:327-331` logs
`failed to start control socket` and continues with `control = None`, so `set_control` is never
called (`main.rs:354-356`) and the WM runs to the end of its life with no control socket at
all. `maverickctl list` then shows it `STALE` (ping fails, `discover.rs:100-111`) while it is
perfectly alive and managing windows, and **no terminal, fresh or not, can ever restart it**.
The realistic trigger is a non-socket file at the socket path — `ControlServer::spawn` only
unlinks when `symlink_metadata(...).file_type().is_socket()` (`control.rs:195-207`) and
otherwise lets `bind` fail with `EADDRINUSE` — or a `set_private_dir` refusal
(`identity.rs:293-304`).

### 5.3 The `ok` / `exec` write race — **confidence: medium**
The reply is written by the *connection* thread (`control.rs:394-396`); the teardown is run by
the *WM* thread (`actions.rs:259` → `136`). If the WM thread reaches `exec` before the
connection thread's `write_all` lands, the accepted stream fd is `SOCK_CLOEXEC` and is closed
by the exec, `write_all` returns `Err`, and the client's `read_line` sees a zero-length read.
`send_command` handles that explicitly: `reply.is_empty()` → `Err(UnexpectedEof, "no reply from
the instance")` (`control.rs:581-586`) → `maverickctl: restart failed: no reply from the
instance`. **The restart happened; the tool reported failure.** The user then retries — and the
retry lands in the 5.1 window. The window is small (the connection thread has nanoseconds; the
WM thread has to run a full X teardown plus file removal first), but it is a real
"reported failure that actually succeeded" path and it is what teaches a user that restart is
untrustworthy.

### 5.4 Session-id churn → forced onto the fragile context path — **confidence: high as a mechanism, but ALREADY FIXED in the working tree**
`identity.rs:269-280` (random sid), `main.rs:282-292` (used when `--session-id` is absent),
`main.rs:297` (exported to children), and — the fix — `actions.rs:157` + `actions.rs:436-443`.
`git diff` confirms `restart_args` is an **uncommitted working-tree change**; the pre-fix code
is what `RESTART-STRESS.md` §5 measured (17 dirs after 15 restarts).

The pre-fix failure chain was: sid changes → the terminal's inherited `MAVERICK_INSTANCE` names
a ficha that no longer exists → `read_meta` → `None` → **fall through to
`ctl/mod.rs:612-647`** → that path is *stricter* than the env branch, and it is the only thing
left. It can fail three ways:
* **race** — the replacement has not written its ficha yet (same window as §3);
* **TTY mismatch** — `i.tty_nr == ctx_tty` is required whenever `ctx_tty != 0`
  (`ctl/mod.rs:621-628`); the WM's `tty_nr` is its *launching* tty (`identity.rs:314-316`,
  `read_proc_tty`), so a `maverickctl` run from a process that is not in the WM's session
  resolves to nothing;
* **ambiguity** — `candidates.len() != 1` refuses outright with
  `multiple instances match` (`ctl/mod.rs:640-646`).
Agent G's F1–F4 exercised only the case where the context path *succeeds* (their harness
launched the WM from the same shell, so DISPLAY and tty both matched), which is why F3/F4
passed and why the prior analysis concluded the env var was harmless. The env var is not
harmless; it is *masked* by a fallback that is itself unreliable.

### 5.5 The context path is unique-match-or-refuse — **confidence: medium-high, independent defect**
`ctl/mod.rs:634-646`. Two live instances that share a `DISPLAY` and a controlling tty — which
is exactly what two `maverickctl session create`s from the same terminal produce, because
`start_maverick` does **not** `setsid`, it only calls `CommandExt::process_group(0)`
(`session/lifecycle.rs:546`) — make *every* subsequent `maverickctl` command in *every* terminal
refuse with `multiple instances match`. This one is permanent and terminal-independent; it is
listed because it is the failure that most often gets misdiagnosed as "the terminal is stale",
and because the fix for 5.1 (wait for the new instance) will make it *more* visible, not less.

### 5.6 X11 connection fd inherited by every spawned child — **confidence: medium-high on mechanism, medium on the libX11 detail**
`restart()` sets `FD_CLOEXEC` on the X fd itself and says why: "we do NOT rely on the
connection layer having set CLOEXEC" (`actions.rs:127-128`, `actions.rs:140-151`). The
connection comes from `XOpenDisplay` (`maverick-x11/src/lib.rs:429`), an Xlib socket, and
Xlib's connection fd is not something this codebase controls. So the X fd is non-`CLOEXEC` for
the entire life of the WM, and every `Command::spawn` — autostart (`main.rs:368-373`) and
`Effect::Spawn` (`actions.rs:168-173`) — hands a duplicate of it to a child.

`fork` shares the *open file description*, so the duplicate is not a second X client: it is a
second reference to the WM's own connection. When the WM later `exec`s and its own fd closes
(the flag it just set), **the X server does not see a disconnect**, because the socket's refcount
is still held by the child. Consequences across N restarts:
* the previous generation's connection stays registered as a live X client for as long as any
  WM-spawned terminal emulator lives, so the X server's client/resource base grows by one full
  WM connection per restart generation;
* the screen handover is *not* blocked — `teardown_x` releases the root select, the EWMH
  properties and the check window explicitly (`mod.rs:574-586`) — but the release is doing
  work the kernel would otherwise have done, and it is doing it over a connection the server
  still considers open.

The control-plane fds are clean: the listener (`control.rs:208`), the accepted streams
(`control.rs:231`, `accept4`+`SOCK_CLOEXEC`) and the hub self-pipe (`hub.rs:129`,
`UnixStream::pair`) are all std-created and therefore `CLOEXEC`, and `detach_from_terminal`
closes its `/dev/null` fd after `dup2` (`lib.rs:450-452`). The trace file is opened and closed
inside `trace::dump` (`trace.rs:294`) and `File::create` truncates, so it does not grow. The
compositor/dev-shm path no longer exists (removed in `f4dd3de`).

### 5.7 Stale `<sid>/` directories accumulate — **confidence: high, cosmetic**
`cleanup_meta` (`identity.rs:435-464`) removes the ficha and the socket but never the
`<sid>/` directory, and nothing else in the tree removes it. With the sid-preservation fix
there is exactly one such directory forever. Pre-fix it is one per restart — Agent G measured
17 entries after 15 restarts and 32 after 30 (`RESTART-STRESS.md` §7.2). `list_instances`
skips an empty one harmlessly (`discover.rs:71-74`), so this is unbounded growth of
inert directories, not a correctness problem. Do not "fix" it with an `rmdir` in
`cleanup_meta`: the replacement process creates the same directory again moments later.

### 5.8 PID reuse and start-time — **confidence: high that there is no defect here**
`is_instance_alive` (`discover.rs:100-111`) pings **and** compares
`read_proc_starttime(pid)` against the ficha's `start_time`. Because `exec` preserves both the
pid and the kernel start time, the post-restart ficha carries exactly the same `(pid,
start_time)` pair as the pre-restart one. Two consequences, both worth stating:
* the check is sound against genuine PID recycling (a recycled pid has a different field 22);
* **the check cannot distinguish "before exec" from "after exec"**, so it is not a restart
  detector and must not be used as one. Any readiness gate added for §5.1 has to be a *socket*
  probe, not a pid/start-time probe. (`session/mod.rs:357-362`'s `wm_is_up` has the same shape,
  plus a permissive `i.start_time == 0` escape.)

## 6. Why does a fresh terminal change behavior?

Not because the terminal is the cause. There are exactly two things a *new* terminal has that
the old one does not, and neither is the terminal itself:

**(a) It is a delay.** Closing and reopening a terminal takes a human several seconds. By then
the unaddressable gap of §3 has long since closed and the replacement is listening. "Close and
reopen the terminal" is a **wait**, not a fix — the correct diagnosis of the user's workaround
is "I waited", and the wait is what actually fixed it. This is the simplest explanation and it
requires no environment story at all.

**(b) Its `MAVERICK_INSTANCE` holds a different *value*.** The variable is exported to children
at `main.rs:297`, so every WM-spawned terminal inherits it. Before the sid fix, a terminal
opened before the restart carried the *old* sid and one opened after carries the *new* sid, and
the old one is pushed onto the fragile context path of `ctl/mod.rs:612-647` while the new one
resolves at `ctl/mod.rs:605` on a file-existence check that trivially succeeds. That is a
genuine terminal-scoped difference — but note what it is: the discriminator is the **value**,
not the presence, which is precisely where the prior analysis went wrong.

Two corollaries that matter for the fix:
* Neither mechanism is a defect *of the terminal*. Any fix that asks the user to reopen a
  terminal is treating a symptom.
* With `--session-id` preservation in place, (b) disappears entirely and only (a) remains —
  which is why the residual, still-unfixed defect is 5.1.

## 7. Why restart can also fail permanently, and no terminal recovers it

* **`exec` failure.** `restart()` swallows it (`actions.rs:158-160`) and then sets
  `state.running = false` (`actions.rs:161`), so the WM exits having already destroyed its
  socket and ficha in `cleanup()`. The instance is simply gone. `maverickctl restart` prints
  `restart exec failed: …` from the *WM's* log, not the client's, so the user sees a successful
  `maverickctl` and no instance afterwards.
* **Control socket that never binds.** `main.rs:326-332`: a bind failure is a warning, not a
  fatal error. The WM runs with `self.control == None` forever
  (`mod.rs:913`, never overwritten at `main.rs:354-356`). `maverickctl` can never reach it,
  and `discover::is_instance_alive` reports it `STALE` while it is running.

Neither is recoverable by reopening a terminal; both are recoverable by an idempotent
`restart` (see §8), because an idempotent restart never leaves the instance without a socket.

## 8. Minimal correct fix

Three changes, in this order. (1) is already in the working tree and must be committed; (2) is
the one that makes restart *repeatable*; (3) removes the silent-wrong-target failure.

**(1) Preserve the session id across the re-exec — already implemented, uncommitted.**
`actions.rs:157` + `actions.rs:436-443`. Without it the socket path and ficha path move on
every restart, the caller's `MAVERICK_INSTANCE` goes stale, and targeting degrades onto the
DISPLAY+TTY fallback. Verified by the three unit tests at `actions.rs:449-488`.

**(2) Make `restart` synchronous on the client — the actual fix for repeatability.**
In `maverick-sys/src/ctl/mod.rs::cmd_simple`, for the `"restart"` arm only, do not return on the
`ok` that `dispatch_line` already produced. After `control::restart(&name)` succeeds, poll
`control::ping(&name)` until it answers or a bounded deadline (5–10 s) elapses, and report the
real outcome. The codebase already contains this exact pattern at
`session/lifecycle.rs:579-593` (`wait_until(START_TIMEOUT, …)` gating on `control::ping` **and**
`control::query(name, "state")`), so this is a copy of an in-repo idiom, not a new mechanism.
Two properties it buys directly:
* a second `restart` can never be issued into the unaddressable gap of §3, because the first one
  does not return until the gap has closed — this is what makes restart **idempotent and
  repeatable**;
* the 5.3 write race is converted from "restart failed" into "restart took N ms", because the
  client's `Err(UnexpectedEof)` is followed by the same bounded wait.
Gate on the **socket**, not on pid/start-time: §5.8 shows the liveness check cannot see the
exec. `ping` is the correct probe, and a `query state` check on top of it (as
`lifecycle::start_maverick` does) additionally proves the event loop is running, not just that
the listener is bound.

**(3) Make the `MAVERICK_INSTANCE` branch liveness-aware.**
`ctl/mod.rs:604-608` currently returns a sid on file existence alone. Add the ping the
`discover` path already performs — i.e. accept the env value only when
`read_meta(&env).is_some() && control::ping(&env).is_ok()` — so a ficha with no listener
falls through to the context path instead of being returned and failing at `connect` with no
recovery. This is a one-condition change and it closes the one branch of `resolve_target` that
is not self-correcting.

**Not required, but cheap and worth doing:**
* Set `FD_CLOEXEC` on the X connection fd immediately after `open_x()` in
  `WindowManager::new` (`mod.rs:772`) rather than only inside `restart()`
  (`actions.rs:146-151`), so no `Command::spawn` at `main.rs:368-373` or `actions.rs:168-173`
  ever hands a WM connection to a child (§5.6). The `fcntl` call and its safety argument
  already exist and would simply move.
* Treat a failed `ControlServer::spawn` at `main.rs:326-332` as fatal rather than a warning
  (§7). A WM that cannot be controlled should say so once and exit, not run for hours
  advertising itself as `STALE`.
* Leave the `<sid>/` directory alone (§5.7). With (1) it is bounded at one.

**What the fix deliberately does not do:** it does not touch `resolve_target`'s
unique-match-or-refuse rule (5.5). That is a separate, larger design question about what
"the instance" means when several are live, and folding it into a restart fix would change
`maverickctl`'s targeting policy for every command.

## 9. File:line citation index

**Client → socket**
| Claim | Citation |
|---|---|
| `maverickctl` binary entry | `maverick-sys/src/bin/maverickctl.rs:6-8` |
| `restart` verb dispatch | `maverick-sys/src/ctl/mod.rs:145` |
| `cmd_simple` → `control::restart`, prints success | `maverick-sys/src/ctl/mod.rs:865-897` (call `:872`, print `:889`, error classify `:884-887`) |
| documented resolution precedence | `maverick-sys/src/ctl/mod.rs:17-26` |
| `resolve_target` | `maverick-sys/src/ctl/mod.rs:590-648` |
| env branch gated on `read_meta` only (no ping) | `maverick-sys/src/ctl/mod.rs:604-608` |
| context path: DISPLAY + tty filter | `maverick-sys/src/ctl/mod.rs:612-628` |
| 0 candidates / >1 candidates refuse | `maverick-sys/src/ctl/mod.rs:634-646` |
| `control::restart` | `maverick-sys/src/control.rs:606-609` |
| `send_command`: connect, 2 s read/write timeouts, one line | `maverick-sys/src/control.rs:542-588` (`:559-561`, `:562-573`) |
| zero-byte reply → `UnexpectedEof` | `maverick-sys/src/control.rs:581-586` |
| `subscribe_stream` treats exec-EOF as clean end | `maverick-sys/src/control.rs:649-...` |

**Server → WM**
| Claim | Citation |
|---|---|
| `restart` answered `"ok"` at enqueue, not completion | `maverick-sys/src/control.rs:443-449` |
| reply written by the connection thread | `maverick-sys/src/control.rs:394-396` |
| per-connection thread, 500 ms read timeout | `maverick-sys/src/control.rs:337-403`, `:348` |
| accept loop, 50 ms poll, `MAX_CONCURRENT` gate | `maverick-sys/src/control.rs:220-272` |
| `ControlServer::spawn`: stale-socket unlink (socket only), `bind`, `chmod 0600` | `maverick-sys/src/control.rs:175-279` (`:195-212`) |
| `ControlServer::shutdown` / `Drop` unlink the socket, thread not joined | `maverick-sys/src/control.rs:282-296`, `:299-303`; rationale `src/backend/x11/teardown.rs:146-150` |
| `push_command` wakes the poll loop via self-pipe | `maverick-sys/src/hub.rs:153-163` |
| `drain_commands` drains pipe then queue | `maverick-sys/src/hub.rs:250-259` |
| `drain_control` runs `Restart` → `self.restart()` | `src/backend/x11/actions.rs:251-289` (`:259`) |

**Teardown → exec**
| Claim | Citation |
|---|---|
| `restart()` | `src/backend/x11/actions.rs:131-162` |
| `cleanup()` first | `src/backend/x11/actions.rs:136` |
| `FD_CLOEXEC` on the X fd, explicit, with the "do not rely on CLOEXEC" rationale | `src/backend/x11/actions.rs:127-128`, `:140-151` |
| `restart_args` preserves the sid | `src/backend/x11/actions.rs:157`, `:436-443`; tests `:449-488` |
| `Command::exec` (in place, same pid), failure swallowed | `src/backend/x11/actions.rs:158-161` |
| `cleanup()` → `shutdown(Clean)` | `src/backend/x11/mod.rs:606-608`, `:492-545` |
| X half gated on a live connection | `src/backend/x11/mod.rs:497-505`, `src/backend/x11/teardown.rs:67-105` |
| root event mask → `NO_EVENT` (SubstructureRedirect released) | `src/backend/x11/mod.rs:574-577` |
| EWMH root props deleted, check window destroyed | `src/backend/x11/mod.rs:583-586` |
| `flush()` — the request that makes the release real | `src/backend/x11/mod.rs:594` |
| local half: `cleanup_meta`, then `drop(control.take())` | `src/backend/x11/teardown.rs:151-176` (`:163`, `:169`) |
| `cleanup_meta` unlinks ficha + socket, never the directory | `maverick-sys/src/identity.rs:435-464` |

**New instance**
| Claim | Citation |
|---|---|
| random sid generation | `maverick-sys/src/identity.rs:269-280` |
| random vs `--session-id` | `src/main.rs:148-161`, `:282-292`; `identity.rs:532-558` |
| `MAVERICK_INSTANCE` export | `src/main.rs:294-297` |
| `detach_from_terminal`, no `setsid`, stdin/stdout only | `maverick-sys/src/lib.rs:423-430`, `:431-454` |
| **`write_meta` before `ControlServer::spawn`** | `src/main.rs:318-320` then `src/main.rs:326-332` |
| control-socket bind failure is a warning, WM runs without one | `src/main.rs:327-331`; `self.control` stays `None` (`src/backend/x11/mod.rs:913`, never set at `src/main.rs:354-356`) |
| `set_session_id` after `WindowManager::new` | `src/main.rs:343-352` |
| `launch_args` captured verbatim | `src/main.rs:185-188` |
| screen reclaimed before the tree scan | `src/backend/x11/mod.rs:799`, `:932-933` |
| `check_no_other_wm` selects `SUBSTRUCTURE_REDIRECT` | `src/backend/x11/mod.rs:991-1000` |
| `scan_windows` adopts viewable, non-override-redirect children | `src/backend/x11/manage.rs:88-119` (`:112`) |
| `XOpenDisplay` — Xlib connection, the fd `restart()` must mark | `maverick-x11/src/lib.rs:408-460` (`:429`) |
| X fd handed to every spawned child | `src/main.rs:368-373`, `src/backend/x11/actions.rs:168-173` |

**Discovery / liveness / session manager**
| Claim | Citation |
|---|---|
| `list_instances` scans per-sid dirs, skips missing ficha | `maverick-sys/src/discover.rs:41-94` |
| `is_instance_alive` = ping **and** start-time | `maverick-sys/src/discover.rs:100-111` |
| `read_proc_starttime` (field 22) | `maverick-sys/src/identity.rs:368-388` |
| `runtime_dir` re-reads `XDG_RUNTIME_DIR` per call | `maverick-sys/src/identity.rs:104-113` |
| `sock_path` / `meta_path` keyed by sid, `SUN_LEN` budget | `maverick-sys/src/identity.rs:225-261` |
| `prune_stale` removes fichas whose socket is dead | `maverick-sys/src/discover.rs:155-164` |
| session WM launched with `--session-id <name>` (stable sid already) | `maverick-sys/src/session/lifecycle.rs:519-526` |
| `process_group(0)`, **no** `setsid` — two sessions share a tty | `maverick-sys/src/session/lifecycle.rs:546` |
| readiness gate that `restart` lacks: `wait_until` on ping + `query state` | `maverick-sys/src/session/lifecycle.rs:579-593` |
| `wm_is_up` = alive + ping + start-time, with a `start_time == 0` escape | `maverick-sys/src/session/mod.rs:357-362` |
| session env exports `MAVERICK_INSTANCE` to `exec`/`shell`/`attach` | `maverick-sys/src/session/mod.rs:449-475` |

**Cross-references (other agents, used as corroboration only)**
| Claim | Citation |
|---|---|
| repeated restart did not fail under a hermetic env; sid churn observed pre-fix; empty dirs accumulate | `RESTART-STRESS.md` §3, §5, §7.2 |
| screen-ownership gap, 50–300 ms, `scan_windows` re-adopts | `WINDOW-RESTART-AUDIT.md` §4 |
| the analysis this document replaces | previous `RESTART-ROOT-CAUSE.md` §3, §5, §8 |
