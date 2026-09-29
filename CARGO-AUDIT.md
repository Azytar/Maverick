# CARGO AUDIT — Maverick workspace dependency archaeology

**Agent A · read-only audit · 2026-09-29**
Repo: `/path/to/Maverick-reconstructed` · `maverick` v0.18.4 · resolver 2

**Method.** Every claim below is backed by a command + its output, a `file:line`, or a
`cargo metadata` extract. Scratch analysis scripts and raw command output live in
`/tmp/kilo/cargo-audit/`; nothing in the repo was modified. No `cargo build/check/test/
add/remove/update/fix` was run. `cargo metadata` was run once *with* network because
`--offline` fails at the `ash` download (see §5.1) — that is itself a finding, not a
workaround.

**One caveat that colours the whole report.** `cargo tree -e features` does **not**
render `x11rb/composite` &c. in this workspace, even though they are genuinely
activated (§2.2). Feature claims below are taken from `cargo metadata`'s
`resolve.nodes[].features`, which is authoritative; `cargo tree -e features` is not.

---

## 1. Summary — findings

1. **There is exactly one feature mechanism: `default = ["compositor-opengl"]`, which
   activates an optional dependency plus three x11rb extension features.** No feature
   in the workspace is selected by any other feature except `default`.
   `Cargo.toml:84,97`; `cargo tree --all-features` is byte-identical to
   `cargo tree` (no diff) — see §2.2 and §8-E1.

2. **`maverick-gl` is the *only* optional dependency in the whole workspace**, and it is
   optional in exactly one place. `Cargo.toml:68` (`optional = true`); the only
   `dep:maverick-gl` reference in any manifest is `Cargo.toml:97`.

3. **`maverick-img` is a MANDATORY root dependency and has a live NON-compositor
   caller.** This is the finding that contradicts the campaign premise. The
   root-pixmap wallpaper path is *explicitly the no-compositor path* and it decodes
   images: `src/backend/x11/rootwall.rs:1` ("Root-pixmap wallpaper — the no-compositor,
   feh-style path"), `:65` (`maverick_img::decode(...)`), compiled unconditionally at
   `src/backend/x11/mod.rs:106` (`mod rootwall;`, no `cfg`). It is called from five
   unconditional sites: `src/backend/x11/events.rs:595`, `src/backend/x11/mod.rs:904`,
   `src/backend/x11/mod.rs:1320`, `src/backend/x11/actions.rs:436`,
   `src/backend/x11/actions.rs:481`. It is the only thing keeping `maverick-img` in the
   `--no-default-features` graph: `cargo tree -i maverick-img --no-default-features` →
   `maverick-img <- maverick` (§3, §4).

4. **`maverick-render` is vestigial.** It has zero dependencies, zero implementors of
   its `Renderer`/`Texture` traits, and its *only* consumer in the entire workspace is
   one `pub use` line: `src/backend/renderer.rs:26`, guarded by
   `#[allow(unused_imports)]` at `:25` and admitted in the file's own header
   (`:17`: "no production code imports the types below"). `maverick-gl` does **not**
   depend on it and defines a structurally identical but separate set of the same value
   types (`maverick-gl/src/renderer.rs:45 Rect`, `:264 Filter`, `:284 TextureHandle`,
   `:288 DrawQuad`, `:331 Acceleration`, `:349 RendererInfo`) —
   `rg 'maverick_render' maverick-gl/` → **NONE**. It is mandatory at the root and
   therefore in the no-compositor graph too (§4, §5.2).

5. **`maverick-sys` is mandatory and correct.** It carries signals, instance identity,
   the Unix control socket, and **a second shipped binary, `maverickctl`**
   (`maverick-sys/Cargo.toml` `[[bin]]`, `maverick-sys/src/bin/maverickctl.rs`).
   48 `maverick_sys::` call sites across 7 production files (§4, §5.3).

6. **`maverick-vk` is unreachable from any shipped binary.** `cargo tree -i maverick-vk`
   *errors* — "package ID specification `maverick-vk` did not match any packages" —
   because nothing in the root's resolve graph depends on it.
   `cargo tree --workspace -i maverick-vk` prints the crate with an empty reverse-dep
   set; `cargo tree --workspace -i ash` shows `ash <- maverick-vk` and nothing else. It
   *is* built by `cargo clippy --workspace` / `cargo test --workspace`
   (`.github/workflows/ci.yml:24,26`), and its `ash` dependency is what makes
   `cargo metadata --offline` fail (§5.1).

7. **`compositor-vulkan` is a feature that gates nothing and enables nothing.** Its
   value list is `[]` (`Cargo.toml:99`) and the only occurrence of
   `cfg(feature = "compositor-vulkan")` anywhere in `src/` is a `///` **doc comment**
   (`src/backend/x11/compositor_gl.rs:264`: "…`Vulkan` will be added behind
   `#[cfg(feature = "compositor-vulkan")]`…"). It does not even make `maverick-vk`
   reachable (§2.2, §5.1).

8. **`x11rb`'s `sync` feature is the one misplaced feature flag.** Its only use site in
   the entire workspace is `src/backend/x11/compositor_gl.rs:76`
   (`use x11rb::protocol::sync::{ConnectionExt as _, Fence};`), the XSync fence. It is
   declared *unconditionally* at `Cargo.toml:64` while its three siblings
   (`composite`, `damage`, `xfixes`) are correctly gated behind `compositor-opengl`
   (`Cargo.toml:97`) — and all three of those siblings are used only in
   `compositor_gl.rs` or inside `#[cfg(feature = "compositor-opengl")]` blocks. The
   manifest comment at `Cargo.toml:62` ("Sync is a small, capability-checked X11
   protocol feature used by the optional fence") is accurate but does not say the fence
   is compositor-only (§5.4).

9. **`maverick-x11` is genuinely required and cannot be replaced by "the WM talks to
   x11rb directly".** `x11rb` alone cannot produce the shared `Display*`:
   `maverick-x11/src/lib.rs:408-464` does `XInitThreads` → `XOpenDisplay` →
   `XSetEventQueueOwner(dpy, XCB_OWNS_EVENT_QUEUE)` → `XGetXCBConnection` →
   `XCBConnection::from_raw_xcb_connection(raw, false)`. A bare
   `x11rb::rust_connection::RustConnection::connect` would open a *second* socket with a
   second sequence-number space, defeating the crate's stated invariant
   (`maverick-x11/src/lib.rs:5-7`). Separately, the root's *own* `x11rb` dep is also
   live: 82 `x11rb::` lines across 15 production files (§5.5).

10. **A `--no-default-features` build is already compositor-free at the *code* level
    but not at the *manifest* level.** The stub compositor is a real no-op
    (`src/backend/x11/compositor.rs:28-55`: `Compositor::init` returns `None`, no GL
    context, no X extension, no frame loop), and `compositor_gl.rs` is only compiled
    under `#[cfg(feature = "compositor-opengl")]` (`:21-26`). But `maverick-render`
    (§1.4) and `maverick-img` (§1.3) — both compositor-adjacent — remain **mandatory**.
    The runtime crate count drops only from 16 to 15 (§3).

---

## 2. Real feature graph

### 2.1 Per-crate declared features (from `cargo metadata --format-version 1`)

| Crate | Declared features | `default` | Optional deps | Build-deps | Dev-deps | Target-specific deps |
|---|---|---|---|---|---|---|
| `maverick` (root) | `compositor-opengl`, `compositor-vulkan`, `input-trace`, `window-trace`, `default` | `["compositor-opengl"]` | **`maverick-gl`** (only optional dep in the workspace) | none | `proptest`, `tempfile` | none |
| `maverick-core` | *(none)* | — | none | none | `proptest` | none |
| `maverick-sys` | *(none)* | — | none | none | `proptest`, `tempfile` | none |
| `maverick-x11` | *(none)* | — | none | none | `proptest` | none |
| `maverick-render` | *(none)* | — | none | none | `proptest` | none |
| `maverick-gl` | *(none)* | — | none | none | `proptest` | none |
| `maverick-toml` | *(none)* | — | none | none | `proptest` | none |
| `maverick-img` | *(none)* | — | none | none | `proptest`, `tempfile` | none |
| `maverick-vk` | *(none)* | — | none | none | `proptest`, `maverick-x11`, `x11rb` | none |

**No crate in the workspace declares a single feature except the root.** Every
`maverick-*` library is feature-less; there is no feature architecture below the root,
only the root's four features. There are no `[build-dependencies]` and no
`[target.'cfg(...)'.dependencies]` blocks anywhere in the workspace.

### 2.2 The root feature table and what each entry actually does

`Cargo.toml:82-99`:

```toml
[features]
default = ["compositor-opengl"]
input-trace = []
window-trace = []
compositor-opengl = ["dep:maverick-gl", "x11rb/composite", "x11rb/damage", "x11rb/xfixes"]
compositor-vulkan = []
```

| Feature | Selects | Gates code at | Verdict |
|---|---|---|---|
| `default` | `compositor-opengl` | — | compositor by default |
| `compositor-opengl` | `dep:maverick-gl` + 3 x11rb ext features | `src/backend/x11/compositor.rs:21,25`; `src/backend/x11/events.rs:65,700,718,749`; `src/backend/x11/mod.rs:416,418,420`; `src/core/mod.rs:85`; `src/core/wallpaper.rs:29` | **live, real** |
| `input-trace` | nothing | `src/backend/x11/input.rs:38,397`; `manage.rs:59,596`; `pointer.rs:46,200,380,457,499,506`; `render.rs:81,1157,1302,1484,1493,1540,1555` | live, zero-dep diagnostic |
| `window-trace` | nothing | `src/backend/x11/input.rs:53,402`; `manage.rs:74,628`; `pointer.rs:61,177`; `reconciler.rs:62,238`; `render.rs:95,641,654,1476` | live, zero-dep diagnostic |
| `compositor-vulkan` | **nothing** | **nothing** (only a doc comment at `compositor_gl.rs:264`) | **dead feature** |

**Proof that `compositor-vulkan` selects nothing:**

```
$ diff <(cargo tree --offline) <(cargo tree --all-features --offline)
IDENTICAL (no diff)
```

and, from `cargo metadata --all-features` vs `cargo metadata` (default), the resolved
`x11rb` feature set is *character-for-character identical*:

```
default  x11rb: [allow-unsafe-code, as-raw-xcb-connection, composite, damage, libc,
                  randr, render, shape, sync, xfixes, xkb]
all      x11rb: [allow-unsafe-code, as-raw-xcb-connection, composite, damage, libc,
                  randr, render, shape, sync, xfixes, xkb]
no-default x11rb:[allow-unsafe-code, as-raw-xcb-connection, libc, randr, render,
                  shape, sync, xkb]
```

`compositor-vulkan` is in `--all-features` and changes nothing. It also does **not**
pull `maverick-vk` (see §5.1).

**Proof that `compositor-opengl`'s x11rb entries are real** (the `composite`/`damage`/
`xfixes` triple is absent from `--no-default-features` and present in both other
configurations, above). Note the same table shows `sync` present in **all three**
configurations, including `--no-default-features` — that is finding §1.8, and it is
visible in this one line.

---

## 3. Dependency graph per configuration

Commands: `cargo tree [--no-default-features|--all-features] [-e normal,build] -p maverick`
(cargo 1.x, `--offline` throughout). "Runtime" = `normal`+`build` edges only; dev
edges are listed separately and do **not** ship.

### 3.1 Runtime crate counts (target-filtered, Linux)

| Configuration | Runtime crates | Local workspace crates | External crates |
|---|---|---|---|
| default (`compositor-opengl`) | **16** | 8 | 8 |
| `--no-default-features` | **15** | 7 | 8 |
| `--all-features` | **16** | 8 | 8 |

Exact lists (all unique crate names):

*default* — 16:
`maverick`, `maverick-core`, `maverick-gl`, `maverick-img`, `maverick-render`,
`maverick-sys`, `maverick-toml`, `maverick-x11`, `libc`, `x11rb`, `x11rb-protocol`,
`as-raw-xcb-connection`, `gethostname`, `rustix`, `bitflags`, `linux-raw-sys`

*`--no-default-features`* — 15 (identical minus `maverick-gl`):
`maverick`, `maverick-core`, `maverick-img`, `maverick-render`, `maverick-sys`,
`maverick-toml`, `maverick-x11`, `libc`, `x11rb`, `x11rb-protocol`,
`as-raw-xcb-connection`, `gethostname`, `rustix`, `bitflags`, `linux-raw-sys`

**`maverick-img` and `maverick-render` are in all three.** `maverick-vk` and `ash` are
in **none**.

Full `cargo tree --no-default-features` runtime graph:

```
maverick v0.18.4
├── libc v0.2.186
├── maverick-core v0.18.4
├── maverick-img v0.1.0
├── maverick-render v0.18.4
├── maverick-sys v0.18.3
│   ├── libc v0.2.186
│   └── rustix v1.1.4
│       ├── bitflags v2.13.0
│       └── linux-raw-sys v0.12.1
├── maverick-toml v0.18.3
├── maverick-x11 v0.18.4
│   └── x11rb v0.13.2
│       ├── as-raw-xcb-connection v1.0.1
│       ├── gethostname v1.1.0
│       │   └── rustix v1.1.4 (*)
│       ├── libc v0.2.186
│       ├── rustix v1.1.4 (*)
│       └── x11rb-protocol v0.13.2
└── x11rb v0.13.2 (*)
```

### 3.2 Dev dependencies of the root (identical in every configuration)

`proptest` and `tempfile` (`Cargo.toml:70-80`), declared `proptest.workspace = true` /
`tempfile.workspace = true`. Full dev closure = **37 crates**:

`proptest`, `tempfile`, `bit-set`, `bit-vec`, `bitflags`, `num-traits`, `autocfg`
(build-dep of `num-traits`), `rand`, `rand_core`, `rand_chacha`, `rand_xorshift`,
`getrandom` 0.3.4, `getrandom` 0.4.3, `cfg-if`, `libc`, `ppv-lite86`, `zerocopy`,
`zerocopy-derive`, `rusty-fork`, `fnv`, `quick-error`, `wait-timeout`, `once_cell`,
`fastrand`, `regex-syntax`, `unarray`, `rustix`, `linux-raw-sys`, `errno`,
`windows-sys`, `windows-link`, `proc-macro2`, `quote`, `syn`, `unicode-ident`,
`r-efi` 5.3.0, `r-efi` 6.0.0, `wasip2`, `wit-bindgen`.

Note the deliberate omission documented at `Cargo.toml:71-75`: `maverick-gl` is **not**
repeated under `[dev-dependencies]`, so `cargo test` on a `--no-default-features` build
does not pull the GL backend in. That decision is correct and should be preserved.
`tests/child_lifecycle.rs:129,140,182` uses `maverick_img::decode` from the root's
*normal* dep, which is why removing `maverick-img` (hunk C2) would also require touching
that test.

`cargo tree -d` shows the only duplicates in the graph are `getrandom` 0.3.4 vs 0.4.3
(dev-only) — no runtime duplication.

---

## 4. Per-crate verdict table

| Crate | Optional at root? | In `--no-default` runtime graph? | In default? | In `--all-features`? | Actual users (`file:line`) | Verdict |
|---|---|---|---|---|---|---|
| `maverick-gl` | **YES** (`Cargo.toml:68`, only via `dep:maverick-gl` at `:97`) | **NO** | YES | YES | `src/backend/x11/compositor_gl.rs:59,1162,1398,4376,5150` (whole file is `#[cfg(feature="compositor-opengl")]`, `compositor.rs:21-23`); plus `maverick-gl/tests/*` | **Compositor. Keep as optional; drop when the compositor moves out.** |
| `maverick-render` | no — **mandatory** (`Cargo.toml:50`) | **YES** | YES | YES | `src/backend/renderer.rs:26` **and nothing else in the workspace** except `maverick-render/tests/contract_types.rs:13` | **Dead in the shipped binary. Zero implementors. Redundant with `maverick-gl`'s own type set.** |
| `maverick-img` | no — **mandatory** (`Cargo.toml:49`) | **YES** | YES | YES | `src/backend/x11/rootwall.rs:65,169,209` (**non-compositor**); `src/backend/x11/compositor_gl.rs:63,2504` (compositor); `src/core/wallpaper.rs:20` (unconditional re-export); `tests/child_lifecycle.rs:129,140,182`; `maverick-gl/src/renderer.rs:34` | **WM concern, not compositor-only. KEEP mandatory.** Two live consumers ⇒ do not inline. |
| `maverick-sys` | no — mandatory (`Cargo.toml:47`) | **YES** | YES | YES | 48 lines / 7 files: `src/main.rs:97,284,291,300,301,319,322,326,327,413`; `src/backend/x11/mod.rs:284,305,673-678,993`; `src/backend/x11/actions.rs:303,315,330-340`; `src/backend/x11/hubevents.rs:17`; `src/backend/x11/teardown.rs:26,163,218,230-252,326`; `src/core/ipc.rs:30-509` | **WM/OS correctness. KEEP mandatory.** Also ships `maverickctl`. |
| `maverick-vk` | not a root dep at all | **NO** | **NO** | **NO** | only its own tests (`maverick-vk/tests/{properties,unit,smoke}.rs`) | **Unwired. Contributes 0 to any user binary.** |
| `maverick-x11` | no — mandatory (`Cargo.toml:54`) | **YES** | YES | YES | `src/backend/x11/mod.rs:81,1071`; `compositor.rs:34`; `teardown.rs:27,326`; `compositor_gl.rs:64,1392`; `events.rs:1076,1088`; plus `maverick-gl` (`lib.rs:59`, `glx.rs:11`, `renderer.rs:55,…`) and `maverick-vk/tests/smoke.rs:26` | **WM correctness. KEEP mandatory. Third-party crate with a demonstrated second consumer — do not inline.** |
| `maverick-toml` | no — mandatory (`Cargo.toml:48`) | **YES** | YES | YES | `src/userconfig.rs:51` only | **WM correctness (config). KEEP mandatory.** |
| `maverick-core` | no — mandatory (`Cargo.toml:46`) | **YES** | YES | YES | `src/types.rs:14,15`; `src/core/wallpaper.rs:13,15`; `src/core/present.rs:699`; `src/backend/x11/ewmh.rs:336` | **WM correctness. KEEP mandatory.** |

---

## 5. Suspicious & dead dependency state

### 5.1 `maverick-vk` — dead weight, and it breaks `--offline`

**Unreachable from any build of the product.** `cargo tree -i maverick-vk` (root
package, the workspace's only default member) *fails outright*:

```
error: package ID specification `maverick-vk` did not match any packages
help: a package with a similar name exists: `maverick-gl`
```

With `--workspace` the inverse tree has an **empty** reverse-dep set:

```
$ cargo tree --workspace -i maverick-vk
maverick-vk v0.18.4 (/…/maverick-vk)
        <- (no reverse dependencies)

$ cargo tree --workspace -i ash
ash v0.38.0+1.3.281
└── maverick-vk v0.18.4 (/…/maverick-vk)
```

`cargo tree` (default members = `maverick` only, per `cargo metadata`
`workspace_default_members`) never emits `maverick-vk` or `ash` in any configuration,
including `--all-features`. **It adds nothing to a user binary.**

**It is not code-rot, though.** It is 2 190 lines of `src/` plus 3 test files
(~48 kB of tests) and is built by `.github/workflows/ci.yml:24` (`cargo clippy
--workspace --all-targets`) and `:26` (`cargo test --workspace`). `git log` shows
recent active work (`b0bd490`, `47e1e6d`, `ecf728f`, `89b48c7`, `83db018`). So: *built
and tested, never linked*. That is a policy question (a backend boundary kept warm),
not dead code.

**Operational consequence — the reason this is more than bookkeeping:**

```
$ cargo metadata --offline --format-version 1
error: failed to download `ash v0.38.0+1.3.281`
Caused by:
  attempting to make an HTTP request, but --offline was specified
```

`maverick-vk` is the only reason `cargo metadata --offline` fails in this workspace
(§8-E0). It is a member, so its manifest is always resolved; its `ash` dep is a
**normal** dep, so it is always in the resolve graph. Every member crate would have to
be parsed for `cargo metadata` to answer at all.

**`compositor-vulkan` does not rescue it.** Even if `compositor-vulkan` were wired to
`dep:maverick-vk`, it would still be a no-op today: the feature's value list is `[]`
(`Cargo.toml:99`) and its only `cfg` mention in `src/` is inside a doc comment
(`src/backend/x11/compositor_gl.rs:264`). The manifest already documents it as
"Placeholder for a future Vulkan backend. Not yet wired."

**Recommended:** move `maverick-vk` from `members` to `exclude`. It stays buildable
(`cargo test -p maverick-vk` from its own directory) and stops being resolved by
default. Do **not** delete the crate — it has three test targets and an active commit
history, and the campaign's stated target architecture is "compositor outside the WM",
which is exactly where a Vulkan backend would eventually live.

### 5.2 `maverick-render` — vestigial re-export shim

The whole shim is 29 lines, and it says so itself:

```rust
// src/backend/renderer.rs
12: //! # Status
13: //!
14: //! The seam is declared but not yet spanned: `backend::x11::compositor_gl`
15: //! implements rendering directly against `maverick_gl::Renderer` and does not
16: //! implement this crate's `Renderer` trait, so nothing in the tree implements
17: //! `Renderer` or `Texture` and no production code imports the types below.
...
25: #[allow(unused_imports)]
26: pub use maverick_render::{
27:     Acceleration, DrawQuad, Filter, Rect, Renderer, RendererInfo, Texture, TextureHandle,
28:     VisualDesc,
29: };
```

Its module is declared unconditionally at `src/backend/mod.rs:10` (`pub mod renderer;`),
and nothing in `src/` or `tests/` references `crate::backend::renderer` or any of the
nine re-exported names other than through that file. The GL compositor imports the
*same-named* types from `maverick_gl` instead (`src/backend/x11/compositor_gl.rs:59-62`),
and `maverick-gl` never references `maverick_render` at all.

Corroborating: `maverick-render` is 264 lines, zero dependencies, and its own crate
docs state "Nothing in the workspace therefore depends on this crate except the
re-export in `src/backend/renderer.rs`" (`maverick-render/src/lib.rs:45-47`) and
"#![allow(dead_code)]" (`maverick-render/src/lib.rs:54`) — the crate is
unconditionally suppressing dead-code warnings because nothing uses it.

**Verdict: contributes nothing to any shipped binary in any configuration.** It is
still a mandatory root dep, so `--no-default-features` pays to compile it. The
`#[allow(unused_imports)]` and the "not yet spanned" header are self-documenting
admissions, not a defensible design.

**Constraint check:** does it have a "demonstrated second consumer"? No — one `pub use`,
zero implementors, zero type users. Deleting the *edge* (making the dep optional and
gating the shim behind `compositor-opengl`) is safe. Deleting the *crate* is a
judgement call the campaign should make explicitly, not a Cargo edit.

### 5.3 `maverick-img` — WM concern, not a compositor concern

This is the one the campaign premise most likely gets wrong. `rootwall.rs` is titled and
documented as the non-compositor path:

```rust
// src/backend/x11/rootwall.rs
1: //! Root-pixmap wallpaper — the no-compositor, feh-style path.
2: //!
3: //! The GL compositor draws the wallpaper itself. When it is not running (the WM
4: //! built without a compositor backend, the user disabled it, or GL failed at
5: //! runtime) this module keeps `[wallpaper]` working with plain X11:
6: //!
7: //!   decode (`maverick-img`) → map onto every monitor …
```

and it is not feature-gated: `src/backend/x11/mod.rs:106` is a bare `mod rootwall;`
(with no `#[cfg]` anywhere in the file — `rg 'cfg' src/backend/x11/rootwall.rs` → none).

Call sites, five of six unconditional:

| Call site | Gated? |
|---|---|
| `src/backend/x11/events.rs:595` (monitor reconfig) | no |
| `src/backend/x11/mod.rs:904` (event loop) | no |
| `src/backend/x11/mod.rs:1320` (startup) | no |
| `src/backend/x11/actions.rs:436` (config reload) | no |
| `src/backend/x11/actions.rs:481` (compositor fallback) | no |
| `src/backend/x11/events.rs:713` (compositor dropped) | `#[cfg(feature="compositor-opengl")]` at `events.rs:700` — but that is one of six, not all six |

`rootwall.rs:36-37` (`if self.compositor.is_some() { return; }`) is a *runtime* check,
not a compile-time one.

**So: image decoding is a WM concern for a non-composited WM.** Every headless-X11
install (`install.sh --no-default-features` → `CARGO_FEATURES="--no-default-features"`,
`install.sh:1389`; CI `x11-smoke`, `.github/workflows/ci.yml:49`) still gets a working
`[wallpaper]` through this path. `maverick-img` is 1 426 lines, zero dependencies, and
has **two** live consumers (rootwall + compositor_gl) plus a third inside
`maverick-gl/src/renderer.rs:34` — under the campaign's own rules
("do not inline code that has a demonstrated second consumer") it must not be inlined,
and under "do not recommend deleting a crate that has a live non-compositor caller" it
must not be deleted.

**Verdict: keep `maverick-img` mandatory.** It is not compositor baggage; it is
`feh`-in-a-crate, and `feh` is a WM tool, not a compositor tool.

Secondary note: `maverick_img` shells out to external converters for formats it cannot
decode (`maverick-img/src/lib.rs:1094` `Command::new(&bin)`, stdin null, stdout piped,
stderr null). It is reached from the WM's wallpaper path with a config-supplied path.
`tests/child_lifecycle.rs:115-140` exercises exactly this fallback. Not a Cargo issue;
flagged in §9.

### 5.4 `x11rb` feature `sync` — the one misplaced feature flag

Use site — exactly one in the whole workspace, inside the compositor:

```
src/backend/x11/compositor_gl.rs:76:  use x11rb::protocol::sync::{ConnectionExt as _, Fence};
src/backend/x11/compositor_gl.rs:956: sync_fence: Option<Fence>,
src/backend/x11/compositor_gl.rs:1357: let sync_fence = match require_x_extension(&conn, "SYNC") {
src/backend/x11/compositor_gl.rs:1366: conn.sync_create_fence(root, fence, false)
src/backend/x11/compositor_gl.rs:3258: conn.sync_trigger_fence(fence)
src/backend/x11/compositor_gl.rs:3272: conn.sync_await_fence(&[fence]) / sync_reset_fence(fence)
src/backend/x11/compositor_gl.rs:3692: conn.sync_destroy_fence(fence)
```

`rg 'x11rb::protocol::sync' src/ | rg -v compositor_gl` → **empty**. `compositor_gl.rs`
is only compiled under `#[cfg(feature = "compositor-opengl")]` (`compositor.rs:21-23`).

For comparison, the correctly-gated extensions:

| ext | outside `compositor_gl.rs`? | verdict |
|---|---|---|
| `randr` | YES — `src/backend/x11/mod.rs:75,1446,2203`; `input.rs` (monitor config, mode → frame period) | **WM** |
| `shape` | YES — `src/backend/x11/manage.rs:55`; `render.rs:77`; `events.rs:721` | **WM** |
| `xkb` | YES — `src/backend/x11/mod.rs:76,1558`; `input.rs`; `tests.rs:16` | **WM** |
| `composite` | no (only `compositor_gl.rs:72`) | compositor — correctly gated |
| `damage` | only `events.rs:65-66,700-703`, all inside `#[cfg(feature="compositor-opengl")]` | compositor — correctly gated |
| `xfixes` | only `events.rs:700-703`, inside `#[cfg(feature="compositor-opengl")]` | compositor — correctly gated |
| `sync` | **no** | **compositor — MISTAKENLY unconditional** |

(`x11rb/render` is pulled transitively by `randr`; see
`x11rb-0.13.2/Cargo.toml:108-111` — `randr = ["x11rb-protocol/randr", "render"]`. Not a
free-standing choice.)

**The manifest comment at `Cargo.toml:60-62` is technically true but misleading**: it
justifies `sync` as a "small, capability-checked X11 protocol feature used by the
optional fence" while its three siblings in the very same sentence
(`composite`/`damage`/`xfixes`) *are* moved under `compositor-opengl`. The fence is not
optional in the sense of "off in a non-compositor build" — it lives inside
`compositor_gl.rs`, which does not exist in a non-compositor build.

### 5.5 `maverick-x11` — genuinely required

Argument from the code, not from preference:

* It is the **only** place the process obtains an X connection. `maverick-x11/src/lib.rs:408`
  `open_x()` is the sole entry point, doing `XInitThreads()` (`:424`, load-bearing for
  `XDisplay: Send`), `XOpenDisplay(NULL)` (`:427`), `install_silent_error_handler()`
  (`:433`), `XSetEventQueueOwner(dpy, XCB_OWNS_EVENT_QUEUE)` (`:434`),
  `XGetXCBConnection(dpy)` (`:437`), `XCBConnection::from_raw_xcb_connection(raw, false)`
  (`:448`). The WM calls it at `src/backend/x11/mod.rs:1071`.
* "Talk to x11rb directly" is not an available alternative that preserves behaviour.
  `x11rb::xcb_ffi::XCBConnection` is *constructed from* an existing
  `xcb_connection_t*`; the pure-Rust `RustConnection` would open a **second** socket
  with its own sequence-number space and event queue, which the crate docs explicitly
  design against (`maverick-x11/src/lib.rs:5-7`: "so there is exactly one
  sequence-number space and one event queue").
* The Xlib `Display*` is separately needed by `maverick-gl` for GLX
  (`maverick-gl/src/lib.rs:59` re-exports `XDisplay`; `renderer.rs:1398` in
  `compositor_gl.rs` re-wraps the same pointer for `maverick_gl::XDisplay::from_raw`).
  A x11rb-only WM could not hand GLX a display at all — but a x11rb-only WM would
  have no GLX, which is the point of the target architecture, so this is the *only*
  thing the compositor removal would eventually orphan (and even then, Xlib's
  `XOpenDisplay` is still the only way to get the `Display*` some WMs need for
  `XSetLocaleModifiers`/`XrmGetStringDatabase`; that is `[UNVERIFIED]` for this codebase
  because no current code path uses it).
* The root's **own** `x11rb` dep is *also* live, independently: 82 `x11rb::` lines
  across 15 production files (`mod.rs` 19, `compositor_gl.rs` 24, `events.rs` 6,
  `input.rs` 6, `render.rs` 6, `pointer.rs` 5, `atoms.rs` 3, `rootwall.rs` 3,
  `trace.rs` 2, `ewmh.rs` 2, `manage.rs` 2, `compositor.rs` 1, `struts.rs` 1,
  `tests.rs` 1, `userconfig.rs` 1). Collapsing to `maverick-x11` alone would require
  re-exporting x11rb's `Connection` trait, `Cookie`, and every protocol module the WM
  uses — i.e. re-exporting most of x11rb from `maverick-x11`, which is strictly worse
  than the direct dep. **Keep both.**

### 5.6 `maverick-sys` — correct, and larger than it looks

Not a candidate for deletion. Beyond the 48 call sites, it ships a **second binary**:

```
maverick-sys/Cargo.toml:39-41   [[test]] name = "ctl_props" …
maverick-sys/src/bin/maverickctl.rs:8
    maverick_sys::ctl::main_with_args("maverickctl", args)
```

`install.sh:1478` and `install.sh:1483` build `-p maverick -p maverick-sys` precisely
so both binaries are produced. Its `rustix` feature set is fully justified, with use
sites for every one:

| rustix feature | use site |
|---|---|
| `event` | `maverick-sys/src/lib.rs:481,490,495,500` (`poll`) |
| `fs` | `maverick-sys/src/session/xserver.rs:217,234,242,258,271` |
| `net` | `maverick-sys/src/control.rs:115` (`socket_peercred`) |
| `process` | `maverick-sys/src/identity.rs:135,144` (`getuid`/`getgid`); `session/proc.rs:474,499`; `session/lifecycle.rs:712-716` (`kill_process_group`) |
| `rand` | `maverick-sys/src/identity.rs:286`; `session/xserver.rs:456` (`getrandom`) |
| `std` | crate-wide |

No dead `rustix` feature.

### 5.7 Other things checked and found clean

* **No `[build-dependencies]`** and **no `[target.'cfg(...)'.dependencies]`** anywhere
  in the workspace (`cargo metadata` `dep.target == null` for every edge in every
  crate; §2.1).
* **The root `[workspace.dependencies]` table is fully consumed** —
  `proptest` is referenced by 9 `[dev-dependencies]` blocks (root + all 8 member
  crates) and `tempfile` by 3 (root `Cargo.toml:80`, `maverick-sys/Cargo.toml:49`,
  `maverick-img/Cargo.toml:19`). No dead workspace dep.
* **`maverick-gl`'s `allow-unsafe-code` x11rb feature is required** (it re-exports
  `XConn`/`XDisplay` at `maverick-gl/src/lib.rs:59,72`). Not removable.
* **No version skew hazard**: only two `getrandom` majors, both dev-only
  (`cargo tree -d`).

---

## 6. Proposed minimal dependency graph

Target: `maverick (bin) -> maverick-core, maverick-sys, maverick-toml, maverick-x11,
maverick-img, (+ libc, x11rb)`, compositor living outside the WM correctness path.

| Crate | Disposition | Reason |
|---|---|---|
| `maverick-core` | **KEEP, mandatory** | pure state; `src/types.rs:14` |
| `maverick-sys` | **KEEP, mandatory** | signals/IPC/identity + ships `maverickctl`; §5.6 |
| `maverick-toml` | **KEEP, mandatory** | config; `src/userconfig.rs:51` |
| `maverick-x11` | **KEEP, mandatory** | only X bootstrap; §5.5 |
| `maverick-img` | **KEEP, mandatory** | non-compositor root-pixmap wallpaper; §5.3 |
| `libc` (root) | **KEEP** | 43 `libc::` lines in `src/` (`main.rs:301,449-526` signal names; `actions.rs:218-220` `fcntl`; `trace.rs:135-146` `clock_gettime`) |
| `x11rb` (root) | **KEEP, features = `randr, shape, xkb, allow-unsafe-code`** | 82 call sites; `sync` moves behind the compositor; §5.4 |
| `maverick-render` | **MAKE OPTIONAL** behind `compositor-opengl` (crate itself kept) | zero users, zero implementors; §5.2 |
| `maverick-gl` | **MAKE OPTIONAL** (already is) — then *remove from the root entirely* when the compositor crate becomes a separate workspace | 100% of users are inside `compositor_gl.rs` |
| `maverick-vk` | **MOVE `members` → `exclude`** | unwired; not in any user binary; breaks `--offline`; §5.1 |

**Resulting minimal runtime graph (15 → 14 crates, one workspace crate fewer):**
`maverick`, `maverick-core`, `maverick-img`, `maverick-sys`, `maverick-toml`,
`maverick-x11`, `libc`, `x11rb`, `x11rb-protocol`, `as-raw-xcb-connection`,
`gethostname`, `rustix`, `bitflags`, `linux-raw-sys`.

`maverick-render` is the **only** crate the minimal config drops from a
`--no-default-features` build. That is the whole measured win of this proposal on the
no-compositor path: **-1 crate, -264 lines compiled, and the removal of a
`#[allow(unused_imports)]` lie.** The honest conclusion is that *the current
no-compositor graph is already essentially minimal* — 15 of its 15 crates are
justified. The one real misconfiguration is the `x11rb` `sync` feature (§5.4), which
costs no crate but pulls `x11rb-protocol`'s sync extension module into every
non-compositor build.

**Proposed root `[features]`:**

```toml
[features]
# A composited build is opt-in. The default profile is the plain X11 WM.
default = []
# OpenGL/GLX compositor backend: pulls in maverick-gl, makes the neutral renderer
# seam reachable, and enables the X extensions only the compositor speaks.
compositor-opengl = [
    "dep:maverick-gl",
    "dep:maverick-render",
    "x11rb/composite",
    "x11rb/damage",
    "x11rb/sync",
    "x11rb/xfixes",
]
input-trace = []
window-trace = []
# No `compositor-vulkan`. maverick-vk is not a dependency of anything; a feature
# that selects nothing is a lie in a feature table. Re-introduce it in the crate
# that actually owns the Vulkan backend, not here.
```

**Proposed workspace table:**

```toml
[workspace]
members = [
    "maverick-core",
    "maverick-sys",
    "maverick-x11",
    "maverick-toml",
    "maverick-img",
    "maverick-render",
    "maverick-gl",
]
exclude = ["maverick-vk"]
resolver = "2"
```

**Proposed root `[dependencies]`:**

```toml
[dependencies]
maverick-core  = { path = "maverick-core" }
maverick-sys   = { path = "maverick-sys" }
maverick-toml  = { path = "maverick-toml" }
maverick-img   = { path = "maverick-img" }
maverick-x11   = { path = "maverick-x11" }
libc           = "0.2"
x11rb = { version = "0.13", default-features = false, features = [
    "randr", "shape", "xkb", "allow-unsafe-code",
] }
maverick-gl     = { path = "maverick-gl",     optional = true }
maverick-render = { path = "maverick-render", optional = true }
```

**This proposal requires exactly one source change** to compile: gate
`src/backend/mod.rs:10` (`pub mod renderer;`) behind
`#[cfg(feature = "compositor-opengl")]`, since
`src/backend/renderer.rs:26` is the crate's only `maverick_render` reference.
(That is a `.rs` edit and therefore **outside this agent's scope**; it is listed here
so the manifest change is not applied in isolation.)

---

## 7. Exact required Cargo changes (ordered, per commit)

None of these are applied. Each is a manifest-only change except C1-note.

---

### Commit 1 — `x11rb/sync` is a compositor feature, not a WM feature

**File:** `Cargo.toml`
**Why:** its only use site is the XSync fence inside `compositor_gl.rs`, which does not
compile without `compositor-opengl` (`src/backend/x11/compositor.rs:21-23`;
use site `src/backend/x11/compositor_gl.rs:76`). Today it is enabled in every
configuration, including `--no-default-features` (§2.2, §5.4).

```diff
--- a/Cargo.toml
+++ b/Cargo.toml
@@ -55,11 +55,11 @@
 # `xkb` is used only to *subscribe* to keyboard-map changes (XkbMapNotify /
 # XkbNewKeyboardNotify), which core `MappingNotify` does not always deliver —
 # the keymap itself is still read with core `GetKeyboardMapping`.
 # `allow-unsafe-code` is what makes `x11rb::xcb_ffi::XCBConnection` available so
 # the WM and any renderer backend can share one connection.
-# `composite`/`damage`/`xfixes` are gated behind `compositor-opengl` so a
-# `--no-default-features` build does not pull compositor X extensions. Sync is
-# a small, capability-checked X11 protocol feature used by the optional fence.
+# `composite`/`damage`/`xfixes`/`sync` are gated behind `compositor-opengl` so a
+# `--no-default-features` build does not pull compositor X extensions. `sync`
+# exists in the tree only for the optional XSync fence in
+# `backend::x11::compositor_gl`, which is not compiled without that feature.
 x11rb = { version = "0.13", default-features = false, features = [
-    "randr", "shape", "sync", "xkb", "allow-unsafe-code",
+    "randr", "shape", "xkb", "allow-unsafe-code",
 ] }
```

and:

```diff
@@ -96,4 +96,4 @@
-compositor-opengl = ["dep:maverick-gl", "x11rb/composite", "x11rb/damage", "x11rb/xfixes"]
+compositor-opengl = ["dep:maverick-gl", "x11rb/composite", "x11rb/damage", "x11rb/sync", "x11rb/xfixes"]
```

**Evidence:** §5.4; §2.2 (resolved-feature table: `sync` present in the
`--no-default-features` set).
**Risk:** none if `compositor-opengl` is on (feature unification restores it); the
`--no-default-features` build does not compile `compositor_gl.rs` at all.

---

### Commit 2 — `maverick-render` stops being a mandatory dependency

**File:** `Cargo.toml`
**Why:** zero implementors, zero type users; the single `pub use` is
`#[allow(unused_imports)]`-suppressed and self-described as unused
(`src/backend/renderer.rs:17,25-29`; `maverick-render/src/lib.rs:45-47,54`). §5.2.

```diff
@@ -45,9 +45,12 @@
 [dependencies]
 maverick-core = { path = "maverick-core" }
 maverick-sys = { path = "maverick-sys" }
 maverick-toml = { path = "maverick-toml" }
-maverick-img = { path = "maverick-img" }
-maverick-render = { path = "maverick-render" }
+# The neutral renderer seam. Nothing implements its `Renderer`/`Texture`
+# traits yet, and `maverick-gl` carries its own value types, so the only
+# consumer in the tree is the `backend::renderer` re-export shim — which is
+# itself compositor-facing. Keep it with the compositor, not the WM.
+maverick-render = { path = "maverick-render", optional = true }
+maverick-img = { path = "maverick-img" }
 libc = "0.2"
```

```diff
@@ -96,5 +96,5 @@
-compositor-opengl = ["dep:maverick-gl", "x11rb/composite", "x11rb/damage", "x11rb/xfixes"]
+compositor-opengl = ["dep:maverick-gl", "dep:maverick-render", "x11rb/composite", "x11rb/damage", "x11rb/xfixes"]
```

**File:** `src/backend/mod.rs` — **REQUIRED COMPANION, outside this agent's scope:**

```diff
@@ -8,4 +8,5 @@
 
 pub mod atoms;
+#[cfg(feature = "compositor-opengl")]
 pub mod renderer;
 pub mod x11;
```

**Evidence:** §5.2. **Risk:** none; nothing references `crate::backend::renderer`.

---

### Commit 3 — `maverick-vk` leaves the default resolve graph

**File:** `Cargo.toml`
**Why:** unreachable from any user binary (`cargo tree -i maverick-vk` errors; §5.1),
yet it is the sole reason `cargo metadata --offline` fails
(`error: failed to download 'ash v0.38.0+1.3.281'`). Kept, not deleted — it has 3 test
targets and an active commit history, and the target architecture puts the compositor
(and any future Vulkan backend) outside the WM.

```diff
@@ -2,13 +2,14 @@
 [workspace]
 members = [
     "maverick-core",
     "maverick-sys",
     "maverick-x11",
     "maverick-render",
     "maverick-gl",
     "maverick-toml",
     "maverick-img",
-    "maverick-vk",
 ]
-exclude = []
+# Not a workspace member until something depends on it. As a member it was
+# resolved by every `cargo` invocation (including `cargo metadata --offline`,
+# which failed on its `ash` dependency) while contributing nothing to any
+# binary. Build and test it from its own directory.
+exclude = ["maverick-vk"]
 resolver = "2"
```

**Evidence:** §5.1. **Consequence to accept:** `.github/workflows/ci.yml:24,26`
(`cargo clippy --workspace --all-targets`, `cargo test --workspace`) stop covering
`maverick-vk`. If that coverage is wanted, add a second job with
`cd maverick-vk && cargo test` — a CI edit, not a Cargo edit, so it is listed here and
not proposed as a diff.

---

### Commit 4 — make the non-composited WM the default profile

**File:** `Cargo.toml`
**Why:** the campaign's target is a non-composited WM; today a plain
`cargo build` produces a composited binary and `install.sh` must pass
`--no-default-features` by hand (`install.sh:1389`) to get the thing the campaign
actually wants. **This is a product decision, not a correctness fix** — it inverts the
default for every downstream builder, so it should be its own commit and its own
release note.

```diff
@@ -82,5 +82,5 @@
 [features]
-# Default build includes the OpenGL compositor backend.
-default = ["compositor-opengl"]
+# Default build is the plain X11 WM. The compositor is opt-in:
+#   cargo build --features compositor-opengl
+default = []
```

**Follow-on:** `install.sh:1389` (`CARGO_FEATURES="--no-default-features"` when the
user declines the compositor) becomes a no-op and could be simplified — an `install.sh`
edit, outside this agent's scope, listed for completeness.

**Evidence:** §3.1 (15 vs 16 crates); §1.10 (the stub compositor is already a true
no-op: `src/backend/x11/compositor.rs:28-55`).

---

### Commit 5 — delete the `compositor-vulkan` placeholder

**File:** `Cargo.toml`
**Why:** a feature that selects nothing and gates nothing. `Cargo.toml:99` value list
is `[]`; the only `cfg(feature = "compositor-vulkan")` in `src/` is a `///` doc comment
at `src/backend/x11/compositor_gl.rs:264`. `cargo tree --all-features` is
byte-identical to `cargo tree`. §2.2, §5.1.

```diff
@@ -96,4 +96,3 @@
 compositor-opengl = ["dep:maverick-gl", "x11rb/composite", "x11rb/damage", "x11rb/xfixes"]
-# Placeholder for a future Vulkan backend. Not yet wired.
-compositor-vulkan = []
```

**Evidence:** §2.2, §5.1. **Note:** this must land **after** commit 3, or a
`--features compositor-vulkan` build would break for anyone using it. No such build
exists in the repo. The one textual reference is `src/backend/x11/compositor_gl.rs:264`,
which is a `///` **doc comment** (`/// behind \`#[cfg(feature = "compositor-vulkan")]\`
without touching WM code.`) — it is prose, not an attribute, and `compositor_gl.rs` is
itself only compiled under `compositor-opengl`, so nothing breaks. It does become
stale prose; that is a one-line comment fix, outside this agent's scope.

---

### What is deliberately NOT proposed

* **No change to `maverick-img`.** It has a live non-compositor caller
  (`src/backend/x11/rootwall.rs:65`, ungated, called from 5 unconditional sites) and
  three consumers overall. Deleting or inlining it would break `[wallpaper]` on every
  `--no-default-features` install. §5.3.
* **No change to `maverick-x11`.** It is the only X bootstrap and it has demonstrated
  second consumers (`maverick-gl`, `maverick-vk/tests`). §5.5.
* **No change to `maverick-sys`.** 48 call sites + a second shipped binary. §5.6.
* **No removal of the root's direct `x11rb` dep.** 82 call sites. §5.5.
* **No deletion of `maverick-render` the crate** — only its mandatory *edge*. The
  campaign can decide separately whether an unimplemented trait boundary earns its
  place; that is a code judgement, not a Cargo one.
* **No change to `maverick-gl`'s manifest.** Once the compositor becomes a separate
  workspace member, `maverick-gl`'s own deps (`libc`, `maverick-img`, `maverick-x11`,
  `x11rb`) are correct as they stand.

---

## 8. Evidence appendix

**E0 — `cargo metadata --offline` fails (only because of `maverick-vk`):**
```
$ cargo metadata --offline --format-version 1
error: failed to download `ash v0.38.0+1.3.281`
Caused by:
  attempting to make an HTTP request, but --offline was specified
```
(`cargo metadata` with network succeeds and downloads `ash v0.38.0+1.3.281` and
`zerocopy-derive v0.8.59` — both listed in `Cargo.lock:11-18,458-475`.)

**E1 — `--all-features` ≡ default:**
```
$ diff <(cargo tree --offline) <(cargo tree --all-features --offline)
IDENTICAL (no diff)
```

**E2 — `maverick-vk` has no reverse dependencies:**
```
$ cargo tree -i maverick-vk --offline
error: package ID specification `maverick-vk` did not match any packages
help: a package with a similar name exists: `maverick-gl`

$ cargo tree --workspace -i maverick-vk --offline
maverick-vk v0.18.4 (/…/maverick-vk)

$ cargo tree --workspace -i ash --offline
ash v0.38.0+1.3.281
└── maverick-vk v0.18.4 (/…/maverick-vk)
```

**E3 — `maverick-img` / `maverick-render` / `maverick-sys` survive `--no-default-features`:**
```
$ cargo tree -i maverick-img     --no-default-features --offline
maverick-img v0.1.0  └── maverick v0.18.4
$ cargo tree -i maverick-render --no-default-features --offline
maverick-render v0.18.4 └── maverick v0.18.4
$ cargo tree -i maverick-sys    --no-default-features --offline
maverick-sys v0.18.3 └── maverick v0.18.4
$ cargo tree -i maverick-gl     --offline
maverick-gl v0.18.4  └── maverick v0.18.4
```

**E4 — resolved x11rb features (from `cargo metadata`, authoritative):**

| config | `x11rb` features |
|---|---|
| default | `allow-unsafe-code, as-raw-xcb-connection, composite, damage, libc, randr, render, shape, sync, xfixes, xkb` |
| `--no-default-features` | `allow-unsafe-code, as-raw-xcb-connection, libc, randr, render, shape, sync, xkb` |
| `--all-features` | identical to default |

**E5 — crate counts (target-filtered, Linux):**
```
$ cargo tree -e normal,build --prefix none -p maverick -p maverick-sys  --offline | sort -u | grep -c .
20          # 16 unique crates; the 4 extra lines are `(*)` duplicates
$ cargo tree -e normal,build --prefix none -p maverick -p maverick-sys --no-default-features --offline | sort -u | grep -c .
18          # 15 unique crates
```

**E6 — the vestigial shim, verbatim** (`src/backend/renderer.rs:12-29`):
```
12: //! # Status
14: //! The seam is declared but not yet spanned: `backend::x11::compositor_gl`
15: //! implements rendering directly against `maverick_gl::Renderer` and does not
16: //! implement this crate's `Renderer` trait, so nothing in the tree implements
17: //! `Renderer` or `Texture` and no production code imports the types below.
25: #[allow(unused_imports)]
26: pub use maverick_render::{
27:     Acceleration, DrawQuad, Filter, Rect, Renderer, RendererInfo, Texture, TextureHandle,
28:     VisualDesc,
29: };
```

**E7 — the non-compositor wallpaper path, verbatim** (`src/backend/x11/rootwall.rs`):
```
 1: //! Root-pixmap wallpaper — the no-compositor, feh-style path.
 7: //!   decode (`maverick-img`) → map onto every monitor
28: use crate::core::wallpaper::{compute_wallpaper_rects, WallpaperSource};
35:     pub(super) fn apply_root_wallpaper(&mut self) {
36:         if self.compositor.is_some() {
37:             return; // the compositor paints its own background
65:         let img = match maverick_img::decode(std::path::Path::new(&path)) {
```
ungated: `src/backend/x11/mod.rs:106  mod rootwall;`
callers: `events.rs:595`, `mod.rs:904`, `mod.rs:1320`, `actions.rs:436`,
`actions.rs:481` (all ungated) + `events.rs:713` (gated).

**E8 — `maverick-render` has zero workspace consumers:**
```
$ rg -n 'maverick_render::' . -g '!target'
src/backend/renderer.rs:26:pub use maverick_render::{
maverick-render/tests/contract_types.rs:13:use maverick_render::{Acceleration, DrawQuad, RendererInfo};

$ rg -n 'maverick_render' maverick-gl/
NONE
```

**E9 — the compositor stub is a true no-op** (`src/backend/x11/compositor.rs:21-55`):
```
21: #[cfg(feature = "compositor-opengl")]
22: #[path = "compositor_gl.rs"]
23: mod compositor_gl;
25: #[cfg(feature = "compositor-opengl")]
26: pub(crate) use compositor_gl::*;
28: #[cfg(not(feature = "compositor-opengl"))]
46:     pub fn init(...) -> Option<Self> {
53:         None
```

**E10 — no build-deps, no target-specific deps** (from `cargo metadata`): every
dependency edge in all 9 local packages has `kind ∈ {null, "dev"}` and `target = None`.

**E11 — the only `compositor-vulkan` mention in `src/`:**
```
src/backend/x11/compositor_gl.rs:264:/// `Gl` is the current OpenGL/GLX implementation; `Vulkan` will be added
                                    (a doc comment; no `cfg` anywhere)
```

**E12 — the full census of `#[cfg(feature = ...)]` sites in `src/`:** 41 total, of
which **40 are real attributes and 1 is a doc comment**.

| feature | count | files |
|---|---|---|
| `compositor-opengl` | **11** | `compositor.rs:21,25`; `events.rs:65,700,718,749`; `mod.rs:416,418,420`; `core/mod.rs:85`; `core/wallpaper.rs:29` |
| `input-trace` | 17 | `input.rs:38,397`; `manage.rs:59,596`; `pointer.rs:46,200,380,457,499,506`; `render.rs:81,1157,1302,1484,1493,1540,1555` |
| `window-trace` | 12 | `input.rs:53,402`; `manage.rs:74,628`; `pointer.rs:61,177`; `reconciler.rs:62,238`; `render.rs:95,641,654,1476` |
| `compositor-vulkan` | **1, and it is a doc comment** | `compositor_gl.rs:264` — `/// behind \`#[cfg(feature = "compositor-vulkan")]\` without touching WM code.` |

**None** in `maverick-gl/src`, `maverick-x11/src`, `maverick-render/src`,
`maverick-img/src` — the libraries are feature-less by design.

**E13 — x11rb extension use sites outside `compositor_gl.rs`:**
```
randr    : src/backend/x11/mod.rs:75,1446,2203 ; src/backend/x11/input.rs
shape    : src/backend/x11/manage.rs:55 ; render.rs:77 ; events.rs:721
xkb      : src/backend/x11/mod.rs:76,1558 ; input.rs ; tests.rs:16
composite: (none)
damage   : events.rs:66,703  (both inside #[cfg(feature="compositor-opengl")])
xfixes   : events.rs:703      (inside #[cfg(feature="compositor-opengl")])
sync     : (none)
```

**E14 — `x11rb` upstream feature definitions** (`x11rb-0.13.2/Cargo.toml:108-134`):
```
randr = ["x11rb-protocol/randr", "render"]      # render is transitive, not our choice
sync = ["x11rb-protocol/sync"]
composite = ["x11rb-protocol/composite", "xfixes"]
damage    = ["x11rb-protocol/damage", "xfixes"]
xfixes    = ["x11rb-protocol/xfixes", "render", "shape"]
```

**E15 — CI / installer build commands:**
`.github/workflows/ci.yml:24` `cargo clippy --workspace --all-targets -- -D warnings`
`.github/workflows/ci.yml:26` `cargo test --workspace`
`.github/workflows/ci.yml:49` `cargo build --release --no-default-features -p maverick -p maverick-sys`
`install.sh:1389` `CARGO_FEATURES="--no-default-features"`
`install.sh:1478,1483` `cargo build --release $CARGO_FEATURES -p maverick -p maverick-sys`

**E16 — workspace default member is the root binary only:**
`cargo metadata` → `workspace_default_members: [path+…#maverick@0.18.4]`.
This is why `cargo tree -i maverick-vk` (without `--workspace`) errors in E2.

**Raw artifacts:** `/tmp/kilo/cargo-audit/{metadata.json,md-default.json,md-nodefault.json,md-all.json,tree-default.txt,tree-nodefault.txt,tree-all.txt,tree-features-*.txt,per_crate.txt}`.

---

## 9. Out-of-scope bugs & unverified claims

**Out of scope, documented, NOT fixed, NOT recommended to fix here:**

1. **`install.sh` build-progress denominator is inflated.** `install.sh:1466-1467` counts
   crates with `cargo tree --edges normal,build --prefix none … | sort -u | grep -c .`.
   `--prefix none` keeps cargo's `(*)` repetition markers, so repeated subtrees are
   counted as separate lines: the command reports **18 / 20** where the real unique
   counts are **15 / 16** (§3.1, E5). The bar is therefore pinned at 90 % before the
   last crate compiles. A `sort -u | sed 's/ (\\*)$//' | sort -u` (or
   `--no-dedupe`-aware counting) would fix it. Install-script concern, not Cargo.

2. **`maverick_img` shells out to external converters with a config-supplied path.**
   `maverick-img/src/lib.rs:1094` `Command::new(&bin)`; stdin `/dev/null`, stdout piped,
   stderr `/dev/null` (`:1121-1123`). Reached from `rootwall.rs:65` with a value from
   the user config's `[wallpaper] path`. This is the intended, tested fallback
   (`tests/child_lifecycle.rs:115-140`), so it is not a bug — but it means a
   non-composited Maverick inherits `feh`'s external-process trust model. Worth an
   explicit decision in the architecture document, not here.

3. **`maverick-x11`'s own docs report that its error-handling cell is unwritable from
   production.** `maverick-x11/src/lib.rs:34-52` and `:54-91`: because `open_x` gives
   the event queue to XCB, libXlib never reads protocol errors, so `take_x_error()`
   returns `None` for every request the WM issues, and the tests that would populate it
   are `#[ignore]`d (`:90-91`). The documented mitigation (`checked_void!` +
   `ReplyError::X11Error`) is what the WM actually uses, so this is understood
   behaviour — but the crate advertises an API (`install_silent_error_handler`,
   `clear_x_error`, `take_x_error`) that production cannot meaningfully exercise. It
   also warns that a synchronous Xlib request against a bad drawable kills the process
   via the default I/O handler (`exit(1)`, `:67-76`) — a live footgun for any future
   Xlib call.

**`[UNVERIFIED]` claims — explicitly not determined:**

* Whether a purely x11rb-based WM (no `maverick-x11`) would lose anything Maverick
  actually uses today. `open_x` is the sole bootstrap and I verified no current code
  path calls a synchronous Xlib request that needs the display's own round trip
  (the crate docs assert this and `tests/x_error_signal.rs` measures it), but I did not
  audit whether `XrmGetStringDatabase` / `XSetLocaleModifiers` style Xlib-only features
  are planned.
* Whether the `x11rb/sync` code actually present in a `--no-default-features` build
  has measurable size or warning cost. The feature is not gated by any `cfg` I found, so
  `x11rb-protocol`'s sync extension module is compiled in; whether LTO+`strip=true`
  (§`Cargo.toml:101-106`) removes all of it from the binary is a link-time question I
  did not run (no `cargo build` permitted).
* The link-time contribution of `maverick-render` to the shipped binary. It is compiled
  and its items are `pub use`d in a binary crate, so most likely LTO drops the code, but
  the crate is unquestionably built in every configuration. Claimed here as "compiled,
  almost certainly not linked", not "linked".
* Whether the campaign intends `maverick-render` to be deleted rather than merely
  ungated. Commit 2 only changes the edge; the crate's fate is a code decision.
