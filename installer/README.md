# The Maverick installer

`installer/install.sh` **is** the installer, and it is the only entry point:
`installer/` is what you run.

```bash
./installer/install.sh            # install maverick + maverickctl into $HOME/.local
./installer/install.sh --help     # every option
```

```
$ python3 installer/tests/partition.py
...
PASS: plain output matches the golden in en and es
partition: all suites passed
```

## What it builds and installs

Two binaries, both built from this workspace with `cargo build --release -p
maverick -p maverickctl` and installed into `<prefix>/bin`:

| binary | crate | role |
|---|---|---|
| `maverick` | the root `maverick` package | the X11 tiling window manager |
| `maverickctl` | `maverickctl` | the separate control client |

`maverickctl` is an independent Unix client that talks to a running Maverick
over its control socket. It is a separate binary by design, not a mode of
`maverick`, so it is built by name and installed as its own file. There is no
third binary: the obsolete `maverick-msg` and `maverick-setup` names are absent
from the workspace and a test asserts they never reappear in an install.

The third file in the prefix is not a binary:

| file | role |
|---|---|
| `<prefix>/share/xsessions/maverick.desktop` | the X11 session entry, mode 0644 |

Maverick is not a desktop environment. There is no compositor, no renderer,
no wallpaper or animation subsystem, no panel, launcher, notification daemon
or session manager, and the installer offers no switch for any of them: the
`--with-compositor` / `--without-compositor` / `--no-compositor` /
`--no-default-features` flags are rejected with a message and exit status 2,
because there is nothing to select. The `maverick` package has exactly two
features, `input-trace` and `window-trace`, both diagnostic and both off by
default; the installer never passes `--features`.

Nothing else is installed either. There is no documentation, no man page, no
icon theme, no shell completion, no showcase asset and no demo entry point in
the prefix: those are not in the repository, and the installer does not
fabricate them.

## Where it writes

Inside the prefix (`--prefix DIR`, default `$HOME/.local`, or `--system` for
`/usr/local`):

- `<prefix>/bin/maverick`, `<prefix>/bin/maverickctl` — mode 0755
- `<prefix>/share/xsessions/maverick.desktop` — the session entry, mode 0644

Outside the prefix, only the caller's own files, and only after being asked:

- `${XDG_CONFIG_HOME:-$HOME/.config}/maverick/config.toml` — seeded from
  `config/config.toml`, unless one is already there (`--no-config` skips it,
  answering the overwrite question no keeps yours)
- one marked, self-guarding block in the login file and interactive rc of
  `$SHELL`, when the bin directory is not already on PATH and the caller
  agrees (`--add-path` / `--no-path` answer that question in advance)
- the session file is copied a second time into a display manager's directory
  only when `--xsessions-dir DIR` names it — the one write that deliberately
  leaves the prefix

It never invokes `sudo`, never escalates, never enables a service, never
touches a desktop environment's configuration, and never changes which window
manager your session selects; that is a `wm-setconfig`/display-manager
decision, and the installer prints the session file's path instead.

It is safe to run repeatedly: a second run converges, repairs umask-hostile
permissions, replaces the binaries as a staged set (a failure leaves the
previous set intact) and does not duplicate the `PATH` block. It verifies what
it installed by executing `maverick --version`, `maverickctl --help` and
`maverickctl session --help`, so a partial, stale or broken set is reported as
a failure rather than as a successful install.

The build directory is `CARGO_TARGET_DIR` as given, defaulting to a cache
directory under `$XDG_CACHE_HOME` — never the checkout. The disk-space
pre-flight measures that same directory, since it can sit on a different
filesystem from where the script was invoked. The first build attempt passes
`-C target-cpu=native` and falls back to a plain build if that fails, so the
installed binary is built for the CPU of the machine that compiled it; use a
normal `cargo build --release` for artifacts meant for another machine.

## Layout

| file | lines | functions | responsibility |
|---|---:|---:|---|
| `install.sh` | 820 | 11 | command line, process state, the six steps, `main()` |
| `lib/i18n.sh` | 244 | 4 | language selection, the two message tables, the table audit |
| `lib/ui.sh` | 570 | 30 | everything that draws or asks |
| `lib/setup.sh` | 438 | 13 | platform, toolchain, disk space, X11 probe, PATH |
| **total** | **2072** | **58** | four files, one entry point |

Around them: `tests/partition.py` (307 lines) proves the behaviour,
`lint.sh` (42) checks it statically, and `golden/` (2 × 42) freezes what it
says.

## The contract

Three rules make the split safe, and each one is tested:

1. **Sourcing defines; it does not probe.** Reading a library declares
   functions and constants and does nothing else — no file read, no `$HOME`
   required, no output. All environment probing is inside `i18n_init`,
   `ui_init` and `os_detect`, which `install.sh` calls once, after the command
   line is parsed (so `--lang` and `--no-anim` are known) and before anything
   prints.
   *(tested: `suite_libraries_are_inert`, which sources the libraries with
   `set -euo pipefail` and `$HOME` removed)*

2. **Libraries never decide and never escalate.** `lib/setup.sh` reports a
   missing toolchain or an unwritable path; it never runs `sudo`. `lib/ui.sh`
   prints and returns a status the caller chooses what to do with.

3. **One trap registration, in the entry.** The exit handler and the signal
   handler are registered where the state they manage is created, once, instead
   of being registered twice by two distant sections that had to agree.

## What lives where

`install.sh` keeps only what is genuinely the program's shape:

```
set -euo pipefail
lib guard · source i18n.sh ui.sh setup.sh
command line        (flags, --help, exit 2 contract)
environment         (root, $HOME, prefix, dirs, cleanups, one trap block)
init                i18n_init · ui_init · os_detect
confirm_install
step_deps step_build step_install step_session step_config step_verify
main "$@"
```

Everything else lives out of the way: the banner, spinners, the live step block
and the summary panel in `ui.sh`; platform detection, Rust, disk, the X11 link
probe and the whole PATH feature in `setup.sh`; `t()` and its two tables in
`i18n.sh`.

There is no celebration effect. An installer that finished correctly has
nothing to announce beyond the fact that it finished, so the summary is a title,
the panel, and what the caller has to act on.

## Prompts

There are three, each with a safe default and each answerable in advance:

| prompt | default | pre-answer |
|---|---|---|
| install into `<prefix>` | yes | `--yes` |
| overwrite an existing `config.toml` | no | `--no-config` |
| add the bin directory to `PATH` | yes | `--add-path` / `--no-path` |

Anything else the installer would need to ask about does not exist: there is no
feature to select, so there is no feature question.

## Running it

```bash
bash installer/install.sh --help            # the CLI, from any cwd
bash installer/lint.sh                      # bash -n on all four; shellcheck when present
bash installer/lib/i18n.sh                  # table parity + every key used is defined
python3 installer/tests/partition.py        # everything, ~7s, as a normal user
python3 installer/tests/partition.py golden # just the output snapshots
MAVERICK_UPDATE_GOLDEN=1 python3 installer/tests/partition.py golden   # accept a wording change
```

`tests/partition.py` runs all 16 suites from `tests/install-smoke.py` — the
harness is imported as a module and its `Sandbox` is pointed at this tree, so
the harness file itself is never edited; only `suite_cli_contract`, which names
the entry point by path, is repointed at it. Six more suites express what a
single file could not: library inertness, i18n parity, the missing-`lib/`
error path, syntax and shellcheck, the build command naming both packages and
no features, and the golden output in both languages.

`golden/` is the record of what the installer says: `install.sh` is a
composed entry point, so its output is the contract rather than a diff against
a predecessor. A wording change regenerates it with `MAVERICK_UPDATE_GOLDEN=1`
and shows up as a reviewable diff.

## What guards the tree

`tests/partition.py` and `installer/golden/` are what enforce this layout.
`suite_libraries_are_inert` sources all three libraries under `set -euo pipefail`
with `$HOME` removed, so a probe that leaks into a library is a failing test
rather than a surprise at install time. `suite_missing_library_is_reported`
deletes `lib/` and requires the failure at the top of the entry point. And
`golden/install.{en,es}.txt` freeze the plain output in both languages, so a
change to any line the installer prints is a deliberate, reviewable edit.
