# Window Ownership & Restart Lifecycle Audit

**Scope**: Window ownership, restart behavior, ICCCM/EWMH lifecycle implications.  
**Date**: 2026-09-29  
**Verdict**: The restart lifecycle is **mostly correct** but has a **gap** where no WM owns the screen, and **does not use WM_Sn selection** (which is acceptable for a SUBSTRUCTURE_REDIRECT-based WM).

---

## 1. What "detach windows" means in this code

"Detach windows" is not a single operation in this codebase. It is the **aggregate effect** of the old instance releasing its WM ownership before the new instance re-establishes it. Specifically:

| Aspect | What happens | Evidence |
|--------|-------------|----------|
| **SUBSTRUCTURE_REDIRECT** | Released (root event mask set to `NO_EVENT`) | `mod.rs:566-569` |
| **Key grabs** | Ungrabbed | `mod.rs:555` |
| **Pointer grab** | Ungrabbed if dragging | `mod.rs:559-562` |
| **Button grabs on clients** | Ungrabbed | `mod.rs:571-573` |
| **EWMH root properties** | Deleted (`_NET_SUPPORTING_WM_CHECK`, `_NET_ACTIVE_WINDOW`, `_NET_CLIENT_LIST`) | `mod.rs:575-577` |
| **check_win** | Destroyed | `mod.rs:578` |
| **Root pixmap** | Freed | `mod.rs:582-584` |
| **Client windows** | **NOT touched** — no unmap, no withdraw, no reparent, no WM_STATE change | `mod.rs:553-588` (entire `teardown_x`) |

**Conclusion**: "Detach windows" means the WM **releases its management authority** (grabs, SUBSTRUCTURE_REDIRECT, EWMH properties) but **leaves client windows exactly as they are** — still mapped, still with their WM_STATE property, still children of root.

---

## 2. Does restart unmap/withdraw/reparent/lose WM ownership/lose selection/close X11 connection/leave clients unmanaged?

### (a) Unmap windows? **NO**
- `teardown_x` does not call `unmap_window` on any client. (`mod.rs:553-588`)
- The only `unmap_window` calls in the codebase are in `unmanage` (`manage.rs:680`), which is not called during teardown.

### (b) Withdraw windows? **NO**
- There is no `withdraw_window` call anywhere in the teardown path.
- Windows remain in their current map state (typically `Viewable`).

### (c) Reparent windows? **NO**
- There is no `reparent_window` call in `teardown_x`.
- Clients remain children of the root window.

### (d) Lose WM ownership (SUBSTRUCTURE_REDIRECT released)? **YES**
- `teardown_x` sets root event mask to `NO_EVENT`, which releases `SUBSTRUCTURE_REDIRECT`. (`mod.rs:566-569`)
- This is the **ICCCM WM election release** — the server removes the redirect when the selecting client disconnects or changes the mask.

### (e) Lose selection ownership (WM_Sn)? **N/A — Maverick does not use WM_Sn**
- There is **no `set_selection_owner` call** anywhere in the codebase.
- There is **no `WM_S0`/`WM_S1` atom** interned. (`atoms.rs:1-224`)
- Maverick uses **SUBSTRUCTURE_REDIRECT** as its sole ICCCM WM election mechanism, not the WM_Sn selection. This is a valid ICCCM approach (the selection is an alternative, not a requirement).

### (f) Close X11 connection? **YES**
- `restart()` sets `FD_CLOEXEC` on the connection fd (`actions.rs:144-149`) and then `exec`s the new binary (`actions.rs:155-157`).
- The old connection is closed by the `exec` (the fd is closed because of `FD_CLOEXEC`).
- The new instance opens a fresh connection via `open_x()` (`mod.rs:731`).

### (g) Leave clients unmanaged? **TEMPORARILY YES — during the gap**
- Between `teardown_x` (release SUBSTRUCTURE_REDIRECT) and the new instance's `check_no_other_wm`/`claim_screen_replacing` (re-claim), **no WM owns the screen**.
- During this gap, client `MapRequest`s are **unhandled** (no WM to receive them).
- The new instance's `scan_windows` (`manage.rs:88-119`) re-manages all existing non-override-redirect, viewable windows, so the gap is temporary.

---

## 3. ICCCM/EWMH lifecycle implications

### ICCCM WM election
- **Mechanism**: SUBSTRUCTURE_REDIRECT on root (`input.rs:72-80`).
- **Teardown**: Released by setting root event mask to `NO_EVENT` (`mod.rs:566-569`).
- **Startup**: Re-claimed by `check_no_other_wm` (`mod.rs:948-957`) or `claim_screen_replacing` (`mod.rs:976-1026`).
- **Gap**: There is a window between release and re-claim where no WM holds the redirect.

### EWMH properties
- **Published at startup**: `_NET_SUPPORTED`, `_NET_SUPPORTING_WM_CHECK`, `_NET_NUMBER_OF_DESKTOPS`, `_NET_CURRENT_DESKTOP`, `_NET_DESKTOP_NAMES` (`input.rs:84-145`).
- **Deleted at teardown**: `_NET_SUPPORTING_WM_CHECK`, `_NET_ACTIVE_WINDOW`, `_NET_CLIENT_LIST` (`mod.rs:575-577`).
- **Republished at startup**: The new instance re-publishes all of them in `setup_root`.

### Client windows during connection close
When the WM closes its X11 connection:
- The X server **does NOT** send `DestroyNotify` to clients (the windows are still alive).
- The X server **does NOT** reparent clients (they are already children of root).
- The X server **does NOT** change `WM_STATE` (it is a property, not server-side state).
- The X server **does** release `SUBSTRUCTURE_REDIRECT` (the selecting client disconnected).
- The X server **does** release any selections owned by the WM (but Maverick owns none).

**Result**: Clients remain in a coherent state — mapped, with their properties intact, but temporarily unmanaged.

---

## 4. The specific code path where window ownership is lost or left undefined

### The gap
```
restart() [actions.rs:129]
  → cleanup() [mod.rs:598]
    → shutdown(Clean) [mod.rs:484]
      → teardown_x() [mod.rs:553]
        → root event mask = NO_EVENT  ← SUBSTRUCTURE_REDIRECT RELEASED
        → flush
      → run_local() [teardown.rs:151]
  → fcntl(FD_CLOEXEC) [actions.rs:144-149]
  → exec() [actions.rs:155-157]     ← PROCESS IMAGE REPLACED

--- GAP: No WM owns the screen ---

New process:
  → WindowManager::new() [mod.rs:724]
    → open_x() [mod.rs:731]        ← New X11 connection
    → check_no_other_wm() [mod.rs:948] or claim_screen_replacing() [mod.rs:976]
      → SUBSTRUCTURE_REDIRECT re-claimed
    → scan_windows() [mod.rs:890]  ← Existing clients re-managed
```

### Duration of the gap
The gap includes:
1. `exec()` syscall (fast, ~1-10ms)
2. Dynamic linking + process startup (~10-100ms)
3. `open_x()` — X11 connection setup (~1-10ms)
4. `check_no_other_wm()` or `claim_screen_replacing()` (~1-150ms, depending on whether another WM is running)
5. `detect_monitors()` (~1-5ms)

**Total**: Typically **50-300ms**, potentially longer on a loaded system or with `--replace` waiting for another WM to yield.

### What can go wrong during the gap
- A client that calls `XMapWindow` during the gap will have its `MapRequest` **unhandled** (no WM to receive it). The window will still be mapped by the server (since no WM is redirecting), but it won't be managed until the new instance's `scan_windows` runs.
- A client that sends a `ConfigureRequest` during the gap will have it **unhandled**.
- A client that exits during the gap will have its `DestroyNotify` **unhandled** (but the window is already gone, so this is harmless).

### Is the gap a problem?
**Mostly no**, because:
- The new instance's `scan_windows` (`manage.rs:88-119`) re-manages all existing non-override-redirect, viewable windows.
- Windows that mapped during the gap are still in the window tree and will be picked up by `scan_windows`.
- The gap is short (typically <300ms).

**But it is a deviation from the ideal ICCCM lifecycle**, which would have no gap at all.

---

## 5. Does the new instance's scan_windows correctly re-manage all existing clients?

### Yes, with caveats
- `scan_windows` (`manage.rs:88-119`) queries the tree, gets window attributes, and manages windows that are `!override_redirect && map_state == VIEWABLE`.
- It correctly handles:
  - Windows on all monitors (via `detect_monitors` + `manage`'s monitor assignment)
  - Windows with `WM_TRANSIENT_FOR` (via `transient_for` + `relink_pending_transients`)
  - Windows with dock struts (via `apply_dock_strut`)
  - Windows with persisted float geometry (via `read_float_prefs`)
- It does **NOT** manage override-redirect windows (by design — these are typically popups, menus, and docks that should not be tiled).

### Potential issues
1. **Override-redirect windows**: Not managed (by design). If a dock is override-redirect, it won't be tracked. However, docks are typically managed via `_NET_WM_WINDOW_TYPE_DOCK` which makes them `is_unmanaged` but still tracked (`manage.rs:268-272`).
2. **Windows that mapped during the gap**: These are in the tree and will be picked up by `scan_windows`. No issue.
3. **Windows that exited during the gap**: These are not in the tree. No issue.
4. **Windows with `WM_STATE` set to `Withdrawn`**: These are not viewable, so they won't be managed. This is correct — a withdrawn window is not supposed to be managed.

---

## 6. Does the new instance re-run setup_root and re-grab everything?

### Yes
- `WindowManager::new` (`mod.rs:724-912`) calls:
  - `setup_root()` (`mod.rs:889`) — re-sets root event mask, re-publishes EWMH props, re-grabs keys, sets up XKB, subscribes to RandR.
  - `scan_windows()` (`mod.rs:890`) — re-manages existing clients.
  - `arrange()` for each monitor (`mod.rs:892-894`) — re-arranges layout.
  - `update_workarea()` (`mod.rs:901`) — publishes workarea.
  - `apply_root_wallpaper()` (`mod.rs:907`) — re-paints wallpaper.

### Yes, it re-arranges
- `arrange()` is called for each monitor after `scan_windows` (`mod.rs:892-894`).
- Each managed window's `manage()` also calls `arrange()` (`manage.rs:570`).

---

## 7. What a correct restart lifecycle would need to preserve

### Ideal ICCCM/EWMH lifecycle
1. **Teardown**: Release SUBSTRUCTURE_REDIRECT, ungrab all grabs, delete EWMH properties, destroy check_win, close X11 connection.
2. **Gap**: No WM owns the screen. (Ideally this would be zero-length, but with `exec`-based restart it is unavoidable.)
3. **Startup**: Open new X11 connection, claim SUBSTRUCTURE_REDIRECT, scan and manage existing windows, re-publish EWMH properties, re-arrange.

### What the current code does well
- Releases all WM-owned resources on teardown (`mod.rs:553-588`).
- Re-claims everything on startup (`mod.rs:724-912`).
- Re-manages all existing clients (`manage.rs:88-119`).
- Preserves client window state (no unmap/withdraw/reparent).
- Preserves float geometry across restart via `_MAVERICK_FLOAT`/`_MAVERICK_GEOM` atoms (`manage.rs:970-1036`).

### Where the current code deviates
1. **The gap**: There is a window where no WM owns the screen. This is unavoidable with `exec`-based restart, but it means:
   - Client `MapRequest`s during the gap are unhandled.
   - Client `ConfigureRequest`s during the gap are unhandled.
   - This is a **minor** deviation — the gap is short and the new instance recovers cleanly.

2. **No WM_Sn selection**: Maverick does not acquire the WM_Sn selection. This is **not a bug** — SUBSTRUCTURE_REDIRECT is a valid ICCCM WM election mechanism. However, some EWMH clients may expect `_NET_SUPPORTING_WM_CHECK` to be present at all times, which it is not during the gap.

3. **EWMH property deletion on teardown**: `_NET_ACTIVE_WINDOW` and `_NET_CLIENT_LIST` are deleted on teardown (`mod.rs:576-577`). This is correct per EWMH (the WM is no longer managing windows), but it means EWMH clients that cache these properties will see them disappear during the gap.

4. **No handoff protocol**: There is no mechanism for the old instance to pass state to the new instance (other than the `_MAVERICK_FLOAT`/`_MAVERICK_GEOM` atoms on client windows). This is **not a bug** — the new instance rebuilds all state from scratch — but it means the restart is a "hard" restart, not a "smooth" handoff.

---

## 8. Summary table

| Question | Answer | Evidence |
|----------|--------|----------|
| Unmap windows? | **No** | `mod.rs:553-588` |
| Withdraw windows? | **No** | `mod.rs:553-588` |
| Reparent windows? | **No** | `mod.rs:553-588` |
| Lose WM ownership? | **Yes** (SUBSTRUCTURE_REDIRECT released) | `mod.rs:566-569` |
| Lose selection ownership? | **N/A** (no WM_Sn used) | `atoms.rs:1-224` |
| Close X11 connection? | **Yes** (FD_CLOEXEC + exec) | `actions.rs:144-157` |
| Leave clients unmanaged? | **Temporarily** (during gap) | `mod.rs:553-588` → `mod.rs:724-912` |
| Re-manage existing clients? | **Yes** (scan_windows) | `manage.rs:88-119` |
| Re-claim SUBSTRUCTURE_REDIRECT? | **Yes** | `mod.rs:948-957` or `mod.rs:976-1026` |
| Re-publish EWMH properties? | **Yes** | `input.rs:84-145` |
| Re-arrange? | **Yes** | `mod.rs:892-894` |

---

## 9. Recommendations

1. **The gap is acceptable** for a tiling WM. The new instance recovers cleanly via `scan_windows`. No action needed.

2. **Consider acquiring WM_Sn selection** for stricter ICCCM compliance. This would allow EWMH clients to detect the WM more reliably. However, this is **not required** — SUBSTRUCTURE_REDIRECT is sufficient.

3. **Consider not deleting `_NET_CLIENT_LIST` on teardown** — instead, leave it as-is so EWMH clients don't see it disappear. The new instance will republish it on startup. This is a **minor** improvement.

4. **Document the gap** in the restart code so future maintainers understand why it exists and why it is acceptable.

5. **The `_MAVERICK_FLOAT`/`_MAVERICK_GEOM` persistence mechanism** (`manage.rs:970-1036`) is a good design — it allows float geometry to survive a restart without a complex handoff protocol. Keep it.

---

## 10. File:line reference index

| File | Lines | What |
|------|-------|------|
| `src/backend/x11/actions.rs` | 129-161 | `restart()` — cleanup + FD_CLOEXEC + exec |
| `src/backend/x11/mod.rs` | 484-537 | `shutdown()` — X half + local half |
| `src/backend/x11/mod.rs` | 553-588 | `teardown_x()` — release WM resources |
| `src/backend/x11/mod.rs` | 598-600 | `cleanup()` — calls shutdown(Clean) |
| `src/backend/x11/mod.rs` | 724-912 | `WindowManager::new()` — startup |
| `src/backend/x11/mod.rs` | 889-894 | `setup_root()` + `scan_windows()` + `arrange()` |
| `src/backend/x11/mod.rs` | 948-957 | `check_no_other_wm()` — claim SUBSTRUCTURE_REDIRECT |
| `src/backend/x11/mod.rs` | 976-1026 | `claim_screen_replacing()` — --replace handover |
| `src/backend/x11/manage.rs` | 88-119 | `scan_windows()` — re-manage existing clients |
| `src/backend/x11/manage.rs` | 121-652 | `manage()` — full client lifecycle |
| `src/backend/x11/manage.rs` | 654-794 | `unmanage()` — reverse of manage |
| `src/backend/x11/manage.rs` | 970-1036 | `read_float_prefs()` / `sync_window_prefs()` — float persistence |
| `src/backend/x11/input.rs` | 67-162 | `setup_root()` — root event mask, EWMH, grabs |
| `src/backend/x11/input.rs` | 269-363 | `grab_keys()` — key grab installation |
| `src/backend/x11/input.rs` | 365-432 | `grab_buttons()` — button grab installation |
| `src/backend/x11/ewmh.rs` | 276-291 | `set_wm_state()` — WM_STATE property |
| `src/backend/x11/ewmh.rs` | 144-159 | `update_client_list()` — _NET_CLIENT_LIST |
| `src/backend/x11/ewmh.rs` | 188-242 | `update_client_list_stacking()` — _NET_CLIENT_LIST_STACKING |
| `src/backend/x11/events.rs` | 67-88 | `on_map_request()` — manage on MapRequest |
| `src/backend/x11/events.rs` | 90-102 | `on_destroy()` — unmanage on DestroyNotify |
| `src/backend/x11/events.rs` | 104-134 | `on_unmap()` — unmanage on UnmapNotify |
| `src/backend/x11/teardown.rs` | 151-176 | `run_local()` — identity ficha + control socket |
| `src/backend/x11/teardown.rs` | 67-69 | `runs_x_half()` — X half gate |
| `src/backend/x11/struts.rs` | 238-278 | `apply_dock_strut()` — dock strut handling |
| `src/backend/x11/rootwall.rs` | 35-87 | `apply_root_wallpaper()` — root pixmap |
| `src/backend/atoms.rs` | 1-224 | Atom definitions (no WM_Sn) |
