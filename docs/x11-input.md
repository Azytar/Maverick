# X11 focus and game input

Maverick selects a managed window for borders, stacking and `_NET_ACTIVE_WINDOW`.
The X input focus may instead name one of its children or an unmanaged popup.
Pointer capture is a separate resource owned by the grabbing X client. Neither
the logical selection nor a focus border proves keyboard delivery or clipping.

A normal focus transfer between visible clients on the selected monitor and
View is accepted when their bounded transient ancestry or ICCCM group
declarations identify the same application family. The backend adopts the
already granted focus and publishes the selection without another SetInputFocus
or WM_TAKE_FOCUS. A fullscreen overlay still excludes a group peer which is not
its owned transient. Hidden, other-monitor and unrelated clients do not gain
activation authority through that path. WM_CLASS and PID are not used to infer
ownership. ICCCM hints are client declarations on a shared X server, not an
authenticated security boundary.

Divergent FocusIn notifications are reconciled after draining the event batch.
The server's current focus, rather than an obsolete notification, decides
whether adoption or repair is needed. Grab-mode and child transitions do not
invalidate the logical selection. An application that intentionally changes
the input window while a key is down must handle the corresponding X11 focus
transition; its release follows the new input owner. Maverick does not fabricate
press/release events to hide a client handoff.

Direct SetInputFocus applies the WM's current selection with CurrentTime. A
cached input timestamp can become stale after internal child focus changes
that do not generate a top-level Normal event. WM_TAKE_FOCUS has different
semantics: ICCCM prohibits CurrentTime in that message. Maverick changes a
private property on its supporting window, consumes the server's PropertyNotify
timestamp for the latest outstanding offer and ignores replies to superseded
offers. Requests are flushed before waiting for their responses. No polling or
repair timer is required.

Wine's usual clipping path uses an override-redirect InputOnly window and
XGrabPointer with that window as `confine_to`. This does not require moving the
keyboard focus into the clipping window. Applications release and reacquire
their own capture when their input mode or foreground status changes. Maverick
does not issue an ungrab on behalf of another client.

## Protocol regressions

Build the release binary first. Test development packages on Debian/Ubuntu are
`libx11-dev`, `libxi-dev` and `libxtst-dev`; the project itself needs no new Rust
dependency for these changes.

```bash
python3 tests/xvfb-input.py --case protocol
python3 tests/xvfb-input.py --case ownership
python3 tests/xvfb-input.py --case child
python3 tests/xvfb-input.py --case capture
python3 tests/xvfb-input.py --case capture --exclusive
python3 tests/xvfb-wine-focus.py --wm target/release/maverick
```

The capture probe uses XTEST and queries core and XI2 raw events on its own
connection. It checks individual and held W/A/S/D, W+A/W+S/W+D, detectable
autorepeat, Shift/Caps, physical key state after release, pointer boundaries,
keyboard grabs, children, unmanaged popups and capture reacquisition. Borderless
mode is verified through actual border geometry, not just a configure request.
XKB repeats the most recently pressed key in a held combination; XI2 raw tracks
the physical transitions rather than requiring duplicate presses for repeat.

Every run creates and reaps its own server and processes with a private runtime
and explicit configuration. `--wm /path/to/reference/maverick` runs the same
regression against a clean reference binary. `--trace /tmp/input.trace` preserves
the WM trace. `--server xephyr` creates a private parent Xvfb and nested Xephyr;
this is a second X server implementation, still a headless test without a
physical desktop, GPU or game.

The older wine-focus probe checks auxiliary focus and Globally Active offers.
Its name does not mean it runs Wine. It validates the offer timestamp and uses
that timestamp to answer. CI requires successful completion and exit status.

## Reproducing in a desktop session

Record the Maverick commit and binary actually installed, X server/session,
keyboard layout, focus_mouse/warp_cursor settings, monitors, game build and
Steam compatibility runtime. Check Steam's Compatibility settings and the
running process: Counter-Strike: Source can use its Linux/SDL client or Proton.
Do not apply a Wine diagnosis to the native route without checking it.

Set these variables in the existing session launcher before starting Maverick:

```bash
export MAVERICK_TRACE=1
export MAVERICK_TRACE_PATH=/tmp/maverick-game.trace
```

Start the session normally; do not start a second WM on the same display. The
trace is saved on clean exit. In Steam, use these launch options for the Proton
run:

```text
PROTON_LOG=1 WINEDEBUG=+timestamp,+event,+keyboard,+x11drv %command%
```

Preserve `steam-<appid>.log` from the user's home directory. Repeat the same
short sequence in windowed, borderless and fullscreen modes: click the game,
hold and release W/A/S/D individually, hold W+A/W+S/W+D for two seconds, repeat
with Shift and Caps, move toward each boundary, open/close an overlay or dialog,
switch away/back, and repeat the keys. Note which step first fails. Restart or
rename the logs between runs so modes are not mixed. For the native client,
collect its SDL/game log and X events instead of claiming a Proton log exists.

Maverick's trace records focus mode/detail, selected-client transfers, pending
offers and their X server timestamps. It does not receive ordinary game keys
which the server delivers directly to the client. Use the Wine keyboard/event
log, or the controlled XI2 probe, for those events. `ns` in the WM trace is a
monotonic nanosecond clock; `x_time_ms` is a wrapping 32-bit X server clock. Wine
log timestamps have their own origin. Correlate window IDs, event order and
known input actions before comparing intervals; do not equate those origins.

For a protocol-level recording when Wine's log does not show a grab result,
install `xtrace`, verify that display `:92` is unused (`xdpyinfo -display :92`
must fail), and make a separate short run with these Steam launch options:

```text
PROTON_LOG=1 WINEDEBUG=+timestamp,+event,+keyboard,+x11drv xtrace -D :92 -o /tmp/game-x11.log -- %command%
```

Choose another unused display if necessary. This introduces an X protocol proxy
and may change rendering or timing; compare with the direct run. Preserve
SetInputFocus, WM_TAKE_FOCUS, GrabPointer replies, UngrabPointer, XISelectEvents,
XI key/motion and ConfigureNotify records. A failed grab, explicit client
release, changed focus owner and geometry change are distinct causes. If only
the proxy run fails, its result cannot establish the original game's bug.

Xvfb/Xephyr results establish the X11 contracts checked by their probes. They do
not certify Blood Strike, Steam overlays, a particular Proton fork, GPU input
latency or the behavior of a physical keyboard.
