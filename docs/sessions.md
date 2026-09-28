# Maverick Sessions

A **session** is a whole graphical unit: an X server, a Maverick, the
applications launched into it, a control socket, logs and a lifecycle. It is
named, reproducible, and controllable from one tool.

```bash
maverickctl session create debug --resolution 1280x720
maverickctl exec debug alacritty
maverickctl window list debug --json
maverickctl session stop debug
```

The idea is not "run Maverick again". It is that one Maverick instance can be a
unit another tool can create, observe, drive and destroy, without touching the
session the user is looking at.

## What a session is

```text
session "debug"
├── X server          real, nested, its own display and its own X cookie
├── Maverick          any binary, any arguments, any working directory
├── applications      everything `maverickctl exec` launched
├── control socket    per-user, 0600, peer-credential checked
├── logs              independent of every other session
└── lifecycle         independent, recorded, reapable
```

On disk, one directory per session under `$XDG_RUNTIME_DIR/maverick`:

```text
$XDG_RUNTIME_DIR/maverick/          0700
└── debug/                          0700
    ├── control.sock                0600   the WM's control channel
    ├── debug.json                  0600   the WM's identity record
    ├── session.json                0600   the session record — maverickctl's
    ├── Xauthority                  0600   the display cookie
    ├── maverick.log                0600   the WM's stderr
    ├── exec.log                    0600   the output of `exec`ed programs
    └── xserver.log                 0600   the X server's stderr
```

Two files with one writer each, deliberately. The window manager is the authority
on what is *running*; the session manager is the authority on what the session
*is*. Neither rewrites the other's half, so a `session create` and a window
manager restart cannot lose each other's update — which a single merged file
would make possible.

## The architecture rule

Maverick remains the authority on the state of the window manager. Every
operation that changes it goes one way:

```text
maverickctl → IPC → Maverick → Action → DesiredState → Reconciler → X11
```

`maverickctl` is a control interface, not a second window manager. It has no
`XMoveWindow` and no `XMapWindow`: `maverickctl window float debug 0x42003`
becomes the action `float_window 0x42003`, and that action runs the *same*
command the `Mod4+F` keybinding runs. A tool and a keypress cannot reach
different code, so there is no second implementation of "float a window" to keep
in step.

What the session manager owns is the *lifecycle* — an X server, a process
graph, a cookie, a display number — and none of that is window state.

The X server, the window manager and the display are fully accounted for: a
session that loses any of them releases what it was holding, and no display
number is left claimed by a process nothing owns. The process *graph* is
narrower than the phrase suggests — see [Limitations](#limitations) for what
`stop` does not reach.

## Commands

### Lifecycle

```bash
maverickctl session list [--json]
maverickctl session create <name> [options]
maverickctl session status <name> [--json]
maverickctl session start   <name>
maverickctl session stop    <name>
maverickctl session restart <name>
maverickctl session kill    <name>
maverickctl session remove  <name> [--force]
```

`session create` options:

| Option | Meaning |
| --- | --- |
| `--resolution <WxH>` | the session's screen size (default `1280x720`) |
| `--refresh-rate <Hz>` | requested refresh rate; Xephyr honours it, Xvfb has no such switch |
| `--backend <xephyr\|xvfb>` | the nested X server to run |
| `--binary <path>` | the Maverick binary (default: `maverick` on `$PATH`) |
| `--cwd <path>` | working directory for Maverick |
| `--debug` | run at debug level — this is what makes `logs` worth reading |
| `--no-compositor` | run without the compositor, to compare against a session that has one |
| `-- <args…>` | everything after `--` is passed to Maverick, verbatim |

A bare name is a `$PATH` lookup and a value with a `/` is a path, so
`--binary maverick` and `--binary ./target/debug/maverick` both mean what they
look like. A relative path is anchored to the directory it was typed in before
the session runs with a different one.

```bash
maverickctl session create debug \
    --binary ./target/debug/maverick \
    --resolution 1280x720 \
    --cwd ~/Descargas/Maverick \
    -- --debug --log-level trace
```

### Running things in a session

```bash
maverickctl exec <session> <program> [args…]     # detached; prints the pid
maverickctl exec <session> … --wait              # block, propagate the exit status
maverickctl exec <session> … --inherit           # output on your terminal
maverickctl shell <session> [command…]           # a shell in the session's environment
maverickctl attach <session> [command…]          # as shell, announcing display and session
```

Everything after the program word is the program's, unfiltered:
`maverickctl exec debug alacritty --json` runs `alacritty --json`. A bare `--`
before the program word hands the rest over untouched.

`exec` puts the program in its own process group and records that group in the
session. While the session is running, that record is the only thing that keeps
an `exec`ed program findable by `process list` after this one-shot CLI exits and
the kernel reparents it to init, where a parent-pointer walk can no longer reach
it. It is actionable ownership state rather than history, so it is cleared when
the session stops — see [Limitations](#limitations) for what that means for a
program still running.

A detached program's output goes to the session's `exec.log` by default rather
than to the caller's terminal: a caller whose stdout is a pipe nobody reads
leaves the program blocked on a full buffer, and a caller on a terminal has its
screen scribbled on by a background job.

The session's environment is set for you:

```text
DISPLAY           the session's X server
XAUTHORITY        the session's cookie file
MAVERICK_SESSION  the session name
MAVERICK_INSTANCE the session id (so maverickctl targets this session)
MAVERICK_LOG      debug when the session was created with --debug
```

`XDG_RUNTIME_DIR` is deliberately **not** overridden: the control socket lives
under it and the session has to be reachable from where the user's own tools look.

### Processes

```bash
maverickctl process list <session> [--json]
maverickctl process inspect <session> <pid> [--json]
maverickctl process kill <session> <pid> [--force]
```

The tree is the union of up to three cheap sets, all read from
`/proc/<pid>/stat`: descendants of the X server, descendants of the window
manager, and every process whose process group this session registered. The
third is what catches `exec`ed programs, and it only applies while the session
still owns a live root — a stopped session owns no processes, so it claims
nothing, however many group ids its record carries. A process this session does
not own is never signalled — `process kill` and `process list` derive both from
one function, so they cannot disagree about what a session contains.

### Windows

```bash
maverickctl window list <session> [--json]
maverickctl window inspect <session> [<window>]
maverickctl window focus <session> <window>
maverickctl window close <session> <window>
maverickctl window move <session> <window> <left|right|up|down>
maverickctl window float <session> <window>
maverickctl window fullscreen <session> <window>
```

A window is addressed by its X11 id (`0x42003`) or by a name matched against the
class, the instance and the title — exact before substring. An ambiguous name is
**refused** with the candidate ids rather than resolved to a guess: taking the
first of three windows called `xterm` is a coin flip between the user's own
windows, and the ids are printed precisely so a caller can pick one.

Omit the window to act on the focused one.

`window list --json` carries each window's `_NET_WM_PID`, which is the link from
a window to a process in the session's process list.

### Layout

```bash
maverickctl camera <session> <left|right|up|down>   # the camera follows the focus
maverickctl resize <session> <+10%|40>              # the focused column
maverickctl layout <session> <column>
```

`resize` takes a percentage because that is what a user means, and a percentage
of what the user can see needs no knowledge of the workarea — the conversion
happens next to the layout that owns the number. A bare number is pixels.

### Looking inside

```bash
maverickctl inspect <session> [--json]
maverickctl logs    <session> [-n N] [-f] [--xserver]
maverickctl debug   <session> [--window <id>]
```

`debug` is two real sources: the structured event stream from the window manager,
and the recent tail of the session's own log. Neither alone answers "what is this
session doing" — the stream is ordered and cheap but only carries transitions,
while the log at `MAVERICK_LOG=debug` carries the reconciliation, geometry,
compositor and damage detail. `--window` keeps lines about that window and lines
that name no window, and drops lines about *other* windows: a failure with no
window reference is exactly the one a filter would otherwise hide.

## The user's own session

`main` is not a special case. It is the session whose display is the caller's,
and it is addressable by the same commands:

```bash
maverickctl session list          # includes main
maverickctl inspect main
maverickctl window list main
```

A session this tool did not create has no record — the user started it, so there
is no specification to record — so it is discovered from the window manager's own
identity record and its name is what it was launched with. A window manager
started without `--name` reads as `main`.

## Failure and cleanup

A session that loses a component is cleaned up by the next command that is
already allowed to change something. There is no per-session daemon: a supervisor
would be another long-lived process to know about, kill and account for, and a
window manager already is one.

Read commands (`list`, `status`, `inspect`) never clean up. An agent that polls
them must not be causing side effects, so they report the *derived* state and
leave the session alone. `create`, `start`, `stop`, `restart` and `remove` reap
first; `kill` tears down without reaping first, which reaches the same resources
and additionally drops a crashed X server's claim. `quit` and `exec` also change
things — `quit` stops a managed session through the same path as `stop` — and
`exec` writes a process group into the record.

The failure that matters is a component that died while the rest kept running:
the orphaned X server holds a display that nothing owns, so the next session
cannot use that number. Reaping stops it and records why. The mirror case — the
X server killed uncleanly, leaving its lock and its socket behind — is released
too, since `display_is_free` treats either file as a claim and neither the
`SIGKILL` nor the escalation inside `XServer::stop` reaches the socket.

A creator killed part-way through `create` used to be the worst case, because
the record was written after the readiness wait and so named no server at all.
The record is now written as soon as the pid is known, so the session stays
findable and stoppable whatever kills the creator.

Choosing a display is the one place two creators can collide, because X display
numbers are a flat machine-wide namespace with no allocator. A candidate number
is checked, and then claimed exclusively before anything is spawned, so two
creators racing the same number cannot both proceed. The claim is held until the
X server has published its own pid in `/tmp/.X<n>-lock` — the server telling us
it won — rather than for a fixed interval, and the kernel releases it if the
creator dies, so a crashed create cannot strand a number. The claim file is
`/tmp/.X<n>-mav`, deliberately not the X server's own lock: that path is taken
by the server itself, and pre-creating it makes every spawn fail.

```text
$ maverickctl session list
NAME         DISPLAY  RESOLUTION   PID      MODE    STATE
debug        :3       1024x768     1737993  debug   crashed
```

The logs survive a crash — they are the reason a crashed session is worth keeping
a record of. `session stop` then reaps the orphaned X server, frees the display
and leaves the log; `session remove` deletes the directory.

The reverse order is handled by the window manager itself, because a tool can
only clean up what it is asked to. When the X server dies first, the window
manager runs the half of its teardown that needs no server — removing its
identity record and its control socket, and writing the compositor trace with
`end=x_connection_lost x_teardown=skipped` — so an X server's death is a
reported state rather than a record left behind. It issues no X or GLX request
on the way out, which is the part that matters: a request on a display whose
server is gone does not fail, it reaches libX11's I/O error handler, and that
ends the process without unwinding.

`session remove` refuses to delete a *running* session's record. That would leave
a window manager running with nothing to address it by, which is the orphan the
record exists to prevent.

## Security

A session belongs to the uid that created it, and that uid comes from the kernel:
there is no flag, environment variable or config key that can declare it.

- `$XDG_RUNTIME_DIR/maverick` and each session directory are `0700`.
- The control socket, the X cookie, the identity record, the session record and
  the logs are `0600`. The mode is set when each file is created and re-asserted
  on rewrite, so it never depends on the process umask.
- The socket's owner is read from the kernel inside the server, and every
  connection is checked with `SO_PEERCRED` before a handler thread exists and
  before a byte is read. A rejected peer gets no reader, no writer and no
  protocol. A peer whose credentials cannot be read is treated the same way.
- The display has its own MIT-MAGIC cookie, created fresh per start and written
  straight into the `.Xauthority` format. A client that guesses the display
  number is refused; `maverickctl` reporting the cookie would defeat the point,
  so no command, JSON document or log ever contains it.
- The X server is started with `-nolisten tcp`.

```bash
tests/session-security.sh [session] [probe-user]
```

checks those four boundaries. The cross-user half needs a second account and is
not skipped silently: without one it reports `SKIP` and exits 2, so a green run
cannot mean the boundary was never checked.

## The nested X server

`xserver` is a backend abstraction and Xephyr is its default implementation. The
alternatives were compared against what this feature actually needs — a real
server, a real GLX for the compositor, no privileges, scriptable, present on a
stock install:

- **Nested Xorg** is the most real option and is what a distribution would ship,
  but it needs the Xorg driver modules to bind a driver at all, races the host
  Xorg for DRM master, and wants logind session ownership. On a machine without
  `/usr/lib/Xorg/modules` it cannot be used at all without root and a
  distribution-specific driver package.
- **Xvfb** is a real X server and needs no privileges, but it is *headless*:
  nothing renders into the parent display, so a user cannot see the session they
  just created, and its GLX is software-rasterised or absent, which means the
  real compositor either fails to initialise or runs a path it never runs on real
  hardware. It remains available for headless and CI use.
- **Xephyr** is a real X server whose framebuffer *is* a window on the parent
  display, with real GLX, Composite, Damage and RANDR. It is the only option that
  gives a developer what this feature is for: a second Maverick they can watch,
  at an independent resolution, without touching the primary session.

Nothing in `session` is an Xephyr wrapper. Display allocation, the cookie,
readiness, liveness, teardown and cleanup are backend-independent, so a future
`xorg` backend is a second arm of `Backend` rather than a rewrite.

## Limitations

- **`stop` does not reach programs started with `exec`.** `exec` puts each
  program in its own process group, outside the window manager's group, so
  teardown signals the manager and stops there. Those programs outlive the
  session, and the record's list of their groups is cleared on the way down —
  so `process list` then reports them as belonging to nobody and `process kill`
  correctly refuses to signal them. They are not leaked silently: they keep
  running, visibly, on a display that no longer exists. Making `stop` total
  over the graph the record itself created is the obvious fix and is not done.
- **A process group id is trusted while the session runs.** `pgrps` records a
  number, not the identity of the group it named. If the kernel reissues that
  number to an unrelated process *before* the session stops, the record would
  authorise a signal it should not. Closing this means recording the group
  leader's start time alongside the id, the same pairing `ProcRef` uses
  everywhere else.
- **`session create` on an existing name ignores the arguments it is given.**
  The recorded spec is replayed verbatim, so a `create` naming a live session
  is refused and a `create` naming a stopped one restarts the old
  configuration rather than the new one. It is not an error, and nothing warns;
  remove and recreate, or edit the record, to change a session's shape.
- **`--refresh-rate`** is honoured by the Xephyr backend, whose `-screen` takes a
  `xDEPTHxFREQ` suffix. Xvfb has no such switch, so the value is recorded and
  reported as declared intent rather than faked.
- **Resizing a running session's display** is not implemented. The X server
  chooses its screen at startup, and changing it afterwards needs a RANDR mode
  handshake that neither backend makes simple. `session create --resolution` is
  the supported way to change a session's size; `session restart` replays the
  record, so removing and recreating is the honest answer today.
- **`attach`** is a shell plus the session's display and name, not a
  multiplexer attach. A nested session is a window on the parent display, so
  what a user attaches to is the graphical session; the terminal's job is to be
  inside it. The interface leaves room for a real attacher without changing
  either `attach` or `shell`.
- **`quit` needs a real answer at the prompt.** `--confirm` resolves through
  zenity, kdialog or a TTY, and a confirmation dialog that is reachable but
  unattended is treated as consent.
- **The cross-user security test** needs a second local account. `main` is
  resolved by the caller's `DISPLAY`, so a caller with no `DISPLAY` and several
  running sessions is refused rather than guessed at.

## Testing

```bash
cargo test --workspace          # unit and property tests
tests/session-suite.sh          # end-to-end, real X server and real windows
tests/session-security.sh       # the four security boundaries
```

The end-to-end suite simulates nothing. It exits 77 (the automake convention for
"skipped") with a reason when Xephyr, a parent display or any X client is
missing, so "could not run" is never reported as "passed".
