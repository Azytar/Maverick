# Repeated-restart stress

`maverickctl restart` has to be executable repeatedly, from one shell, without
closing and reopening that shell.

    restart; restart; restart; ...   # must stay valid

## Why the first version of this test could not see the failure

An earlier version of this test reported that repeated restart never fails. It
was wrong, for a specific and checkable reason: it ran every `maverickctl`
invocation from a shell that had `MAVERICK_INSTANCE` unset and whose controlling
TTY satisfied `resolve_target`'s DISPLAY + `tty_nr` context filter. Under those
conditions the context fallback always resolves, so the test never entered the
state a real user is in.

The real topology is different, and it is the one the harness now builds:

* the window manager runs on its **own pty** — the `.xinitrc` / login-shell case;
* the user's terminal runs on a **different pty** and **inherits
  `MAVERICK_INSTANCE`**, because the WM exports it to every child at startup
  (`src/main.rs`, `std::env::set_var("MAVERICK_INSTANCE", &sid)`);
* so from the second restart onward that terminal's copy of the variable points
  at paths that no longer exist, and the DISPLAY+TTY fallback finds nothing,
  because the WM is on a different tty.

Run: `RESTARTS=N tests/xephyr-restart-stress.sh <BIN_DIR>`

## Checks after every restart

| | |
|---|---|
| V1 | the instance answers `maverickctl query tree` |
| V2 | every pre-restart client is still managed, in `_NET_CLIENT_LIST` and in the tree |
| V3 | `_NET_SUPPORTING_WM_CHECK` names a live window |
| V4 | exactly one ALIVE instance in the runtime directory, and its socket answers |
| V5 | the process recorded in the ficha is alive |

Plus the session id before and after, and the command's exit status — an
instance that silently stopped being reachable is a failure even if the window
count is unchanged.

## Results

### Baseline (`0d83275`, before the fix) — reproduces

    pty C restart #1 -> rc=0  health=FAIL  V1=FAIL  ...
    pty C restart #2 -> rc=1  health=FAIL  V1=FAIL
      stderr: maverickctl: no running Maverick instance found for this context

Two distinct failures, and they are the two the fixes target:

* **restart #1 returns `rc=0` but V1 fails.** `restart` is acknowledged by the
  socket thread when the command is *enqueued*; the tool reports success for a
  restart that has not happened yet.
* **restart #2 returns `rc=1` outright.** By then the terminal's inherited
  session id is stale, and the DISPLAY+TTY fallback cannot resolve it.

A reopened terminal buys exactly one successful restart before its own
inherited id goes stale — which is the reported "close and reopen the terminal"
workaround.

### Fixed — 20/20

    first restart that FAILED : 0
    pty C restart #1  .. #20 -> rc=0  V1=PASS V2=PASS V3=PASS V4=PASS V5=PASS
    session id identical across all 20
    20/20 restarts took effect; 0 reported as failures

No terminal was closed at any point. A control matrix in the same run confirms
the confound above is still visible: a shell on the WM's tty with
`MAVERICK_INSTANCE` unset keeps working for reasons that have nothing to do with
restart, which is exactly why the original test passed.

## An intermediate failure worth recording

With the settle wait in place but before the outgoing instance announced its
own departure, a 20-restart run **failed at restart #13**:

    pty C restart #13 -> rc=1  health=PASS  V1=PASS ...

The window manager was healthy; the *command* reported failure. The wait
required the socket to be observed unbound before it would believe the
replacement had arrived, and that window lasts only as long as the replacement
takes to start — milliseconds. Poll for a transient that short and you will
eventually miss it, and then report a completed handoff as a failure.

The fix is the announcement, not a shorter poll: the WM thread marks the hub as
restarting before unbinding, and the socket thread answers `error restarting`
to the read-side commands from that moment. The outgoing instance now ends the
window itself, so a client never has to catch it mid-unbind.
