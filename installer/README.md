# The Maverick installer

`installer/install.sh` **is** the installer. There is no other one: the
monolithic `install.sh` that used to sit at the repository root has been
deleted, and `installer/` is what you run.

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
`maverick`, so it is built by name and installed as its own file.

Maverick is not a desktop environment. There is no compositor, no renderer,
no wallpaper or animation subsystem, no panel, launcher, notification daemon
or session manager, and the installer offers no switch for any of them: the
`--with-compositor` / `--without-compositor` / `--no-default-features` flags
are rejected with a message, because there is nothing to select. The `maverick`
package has exactly two features, `input-trace` and `window-trace`, both
diagnostic and both off by default; the installer never passes `--features`.

## Where it writes

Inside the prefix (`--prefix DIR`, default `$HOME/.local`, or `--system` for
`/usr/local`):

- `<prefix>/bin/maverick`, `<prefix>/bin/maverickctl` — mode 0755
- `<prefix>/share/xsessions/maverick.desktop` — the session entry, mode 0644

Outside the prefix, only the caller's own files, and only after being asked:

- `${XDG_CONFIG_HOME:-$HOME/.config}/maverick/config.toml` — the example
  configuration from `config/config.toml`, unless one is already there
  (`--no-config` skips it, answering the overwrite question keeps yours)
- one marked, self-guarding block in the login file and interactive rc of
  `$SHELL`, when the bin directory is not already on PATH and the caller
  agrees (`--add-path` / `--no-path` answer that question in advance)
- `<prefix>`'s session file is copied a second time into a display manager's
  directory only when `--xsessions-dir DIR` names it — the one write that
  deliberately leaves the prefix

It never invokes `sudo`, never escalates, never enables a service, never
touches a desktop environment's configuration, and never changes which window
manager your session selects; that is a `wm-setconfig`/display-manager
decision, and the installer prints the session file's path instead.

## Layout

| file | lines | functions | responsibility |
|---|---:|---:|---|
| `install.sh` | 780 | 11 | command line, process state, the six phases, `main()` |
| `lib/i18n.sh` | 246 | 4 | language selection, the two message tables, the table audit |
| `lib/ui.sh` | 610 | 32 | everything that draws or asks |
| `lib/setup.sh` | 421 | 13 | platform, toolchain, disk space, X11 probe, PATH |
| **total** | **2057** | **60** | was 1865 lines, 47 functions, in one file |

Around them: `tests/partition.py` (337 lines) proves the behaviour,
`lint.sh` (42) checks it statically, and `golden/` (2 × 44) freezes what it
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
phase_deps phase_build phase_install phase_session phase_config phase_verify
main "$@"
```

Everything else lives out of the way: the banner, spinners, roadmap bar,
celebration effects and summary panel in `ui.sh`; platform detection, Rust,
disk, the X11 link probe and the whole PATH feature in `setup.sh`; `t()` and
its two tables in `i18n.sh`.

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

`golden/` exists because there is no longer a monolith to diff against: it is
the record of what the installer says. A wording change regenerates it with
`MAVERICK_UPDATE_GOLDEN=1` and shows up as a reviewable diff.

## Provenance

The tree was carved out of the old monolith by a one-time extractor whose
assertions were all line ranges into a file that no longer exists. That
extractor was deleted with the monolith; git history is where the cut is
recorded, and the `golden/` snapshots plus `tests/partition.py` are what guard
it now.
