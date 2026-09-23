# Maverick showcase

This directory contains the reproducible technical presentation of Maverick.
It is a real X11 session running in a private Xephyr server, not a generated
mock-up and not a collection of screenshots with a script painted over them.

## What it demonstrates

The showcase is a six-step story:

1. **workspace** — a terminal, a Neovim source window and Firefox form a clean
   three-column mosaic.
2. **ribbon** — a second source window, an offline reference viewer and the live
   monitor extend the ribbon. The focused column moves the camera and exposes
   more work.
3. **tools** — a real browser, editor, terminal and two purpose-built local
   viewers are shown in the resulting scrollable workspace.
4. **legibility** — the focused editor is deliberately compacted with the real
   `Mod+Ctrl+H` shortcut, then expanded with `Mod+Ctrl+L` three times. The
   harness checks the geometry, checks for tiled overlaps and records the
   before/compact/after measurements in `legibility-actions.json`.
5. **floating** — the in-tree `Maverick Monitor` is floated through the real
   `toggle_float` action. Its geometry is compared before and after the tiled
   camera moves; the float must remain stationary.
6. **hero** — the complete composition keeps the readable column width produced
   by the real shortcut flow, several applications, scrolling and the independent
   floating monitor.

The captures are root-window screenshots of Maverick's actual output. The only
post-processing is a deterministic check of the capture dimensions; no windows,
labels or compositor effects are drawn into the image.

## Run it

From the repository root:

```bash
./showcase/run.sh
```

The default reference is `1920x1080`, matching the development display used
for the checked-in captures. A different nested Xephyr size can be selected
when needed:

```bash
./showcase/run.sh --size 1440x900
./showcase/run.sh floating
./showcase/run.sh --list
```

Running a single scene still runs the earlier scenes as prerequisites, so the
result is reproducible and the story is not silently incomplete. Captures are
written to `docs/screenshots/`. JSON state and window-tree evidence is written
to `/tmp/opencode/mav-showcase-evidence/` by default; use `--evidence PATH` to
choose another location.

The run requires a working host `DISPLAY`, `Xephyr`, `xdpyinfo`, `xdotool`,
`xsetroot`, ImageMagick's `import` and `identify`, and built Maverick binaries.
The terminal scene prefers Alacritty and falls back to xterm. Neovim and
Firefox are used when installed; the showcase does not install or download
optional applications. Firefox receives a private profile, `--offline`, a local
`file://` fixture and telemetry-disabled preferences. A terminal reference
viewer is available as an offline browser fallback. The Maverick Monitor is a
normal terminal application that polls `maverickctl query tree`; it is included
to make the float's real state visible.

## Cleanup and isolation

The harness creates private `HOME`, XDG config/cache/data/state/runtime
directories, a private Firefox profile and a private Maverick configuration.
It uses `PR_SET_CHILD_SUBREAPER`, tracks every process it starts, and recursively
reaps the owned process tree on success or failure. It never changes the host
Maverick instance or the user's `~/.config`.

Firefox is launched with a local URL and offline preferences, but the harness
does not claim to be a kernel-level network sandbox for arbitrary third-party
applications. The bundled page itself has no network dependency.

## Known limitations

- Maverick is currently an X11 column WM; the showcase uses its implemented
  `Column` layout and does not claim a Grid layout.
- The reference captures use plain X11 presentation with the compositor disabled
  for deterministic startup. The OpenGL compositor remains a separate,
  driver-sensitive capability and is not substituted with a fake effect here.
- The floating proof is strongest when the selected monitor is at least
  `640x480`; the default `1920x1080` composition is the documented reference.
- A missing optional application changes the exact client mix, but the scene
  still launches a real application and records the actual WM tree instead of
  pretending that an unavailable application ran.
