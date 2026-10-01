#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  MAVERICK — installer (entry point)
#  columnar tiling WM · bilingual (EN/ES)
#
#  Installs the release binaries into a prefix. The default prefix is
#  $HOME/.local, so a normal installation needs no privileges. A system or
#  custom prefix is always explicit, and outside the prefix the installer
#  writes only the caller's own files — the config under ~/.config and, when
#  the bin directory is missing from PATH and the caller agrees, one marked
#  block in a shell startup file. It never escalates privileges on its own.
#
#  The installer is split into three libraries beside this file:
#    lib/i18n.sh   message tables, language selection, table audit
#    lib/ui.sh     everything that draws or asks
#    lib/setup.sh  platform, toolchain, disk space, X11 probe, PATH
#  This file keeps only the command line, the process state, the six steps
#  and main(). Sourcing the libraries defines things and nothing else; the
#  init block below is where the environment is actually read.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

# ── libraries ────────────────────────────────────────────────────────────────
# Resolved from this script's own location, so the entry point works from any
# working directory, and a checkout missing its lib/ says so instead of dying
# halfway through with an unbound variable.
APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
MAVERICK_LIB="$APP_DIR/lib"
for _lib in i18n ui setup; do
    if [[ ! -r "$MAVERICK_LIB/$_lib.sh" ]]; then
        printf 'install.sh: incomplete checkout — %s/lib/%s.sh is missing.\n' \
            "$APP_DIR" "$_lib" >&2
        printf 'Copy the whole installer directory, or clone the repository again.\n' >&2
        exit 1
    fi
done
unset _lib
# shellcheck source=lib/i18n.sh
. "$MAVERICK_LIB/i18n.sh"
# shellcheck source=lib/ui.sh
. "$MAVERICK_LIB/ui.sh"
# shellcheck source=lib/setup.sh
. "$MAVERICK_LIB/setup.sh"

# ── command line ─────────────────────────────────────────────────────────────
LANG_CHOICE="auto"
PREFIX="${PREFIX:-}"
SYSTEM_INSTALL=false
XSESSIONS_DIR=""
YES=false
NO_CONFIG=false
NO_BUILD=false
# "" = ask when the bin directory is missing from PATH, "yes" = do it without
# asking, "no" = never touch a shell startup file.
PATH_CHOICE=""
NO_ANIM="${MAVERICK_NO_ANIM:-0}"
KEEP_LOG=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --prefix|--lang|--xsessions-dir)
            [[ $# -ge 2 && -n "$2" && "$2" != --* ]] || {
                echo "$1 requires a value" >&2; exit 2;
            }
            case "$1" in
                --prefix)         PREFIX="$2" ;;
                --lang)           LANG_CHOICE="$2" ;;
                --xsessions-dir)  XSESSIONS_DIR="$2" ;;
            esac
            shift 2 ;;
        --system)    SYSTEM_INSTALL=true; shift ;;
        --yes|-y)    YES=true; shift ;;
        --no-config) NO_CONFIG=true; shift ;;
        --no-build)  NO_BUILD=true; shift ;;
        --add-path)  PATH_CHOICE="yes"; shift ;;
        --no-path)   PATH_CHOICE="no"; shift ;;
        --no-anim)   NO_ANIM=1; shift ;;
        --keep-log)  KEEP_LOG=true; shift ;;
        --with-compositor|--without-compositor|--no-compositor)
            echo 'Maverick has no compositor. Drop the flag: there is nothing to select.' >&2
            exit 2 ;;
        --no-default-features)
            echo 'There is no default feature to disable: the build has none.' >&2
            exit 2 ;;
        -h|--help)
            cat <<'HELP'
Usage: ./installer/install.sh [options]

Installs maverick and maverickctl into a prefix. The default prefix is
$HOME/.local and needs no privileges. Outside the prefix the installer writes
only your own files — the config under ~/.config and, when the bin directory
is missing from PATH, a marked block in a shell startup file (see --no-path).
It never invokes sudo.

The release set is two binaries built from this workspace: `maverick`, the
window manager, and `maverickctl`, the separate control client.

Options:
  --system         Install into /usr/local (requires write access to it)
  --prefix DIR     Install prefix (default: $HOME/.local)
  --xsessions-dir DIR
                   Also install the X11 session file into DIR. Off by
                   default: display managers usually only read system
                   locations, so this is an explicit opt-in.
  --lang LANG      Force language: en | es | auto (auto-detects $LANG)
  --yes, -y        Skip installer confirmations
  --no-config      Don't create ~/.config/maverick/config.toml
  --no-build       Skip cargo build (use existing target/release/*)
  --add-path       Add the bin directory to PATH in your shell startup files
                   without asking. Offered by default when it is missing.
  --no-path        Never modify shell startup files; print the export line
                   to run instead.
  --no-anim        Disable the installer's own terminal animation, for a
                   piping or recording session (or export MAVERICK_NO_ANIM=1).
                   It reaches no cargo build: Maverick draws through X11 and has
                   no animation subsystem to turn off.
  --keep-log       Keep build log even on success (saved to share/maverick/)
  -h, --help       Show this help

Environment:
  PREFIX=DIR          same as --prefix
  CARGO_TARGET_DIR    cargo build directory; honoured as given
  LANG / LC_ALL       auto language detection
  MAVERICK_NO_ANIM=1  same as --no-anim
  NO_COLOR=1          disable colors

Notes:
  Run as your normal user. Cargo never runs as root, and a prefix you cannot
  write is reported as an error rather than escalated.
  If CARGO_TARGET_DIR is unset it defaults to a cache directory under
  $XDG_CACHE_HOME; the checkout is never used as a build directory.
  Binaries are compiled with -C target-cpu=native for maximum performance
  on THIS machine. They will NOT work on different CPU architectures.
  If the bin directory is not already on PATH, the installer offers to add a
  marked, idempotent block to your shell startup files — the login file of
  your shell plus its interactive rc, all of them inside $HOME. The block
  guards itself, so re-running the installer adds nothing twice, and a
  startup file that already names the directory is left alone. --yes accepts
  the offer, --no-path refuses it.

Examples:
  ./installer/install.sh
  ./installer/install.sh --system
  ./installer/install.sh --prefix /opt/maverick
  ./installer/install.sh --prefix ~/.local --lang es
  ./installer/install.sh --yes --no-anim --keep-log
HELP
            exit 0 ;;
        *) echo "unknown option: $1" >&2; echo "try: ./install.sh --help" >&2; exit 2 ;;
    esac
done

# ── environment ──────────────────────────────────────────────────────────────
# Every filesystem change below is a plain command: there is deliberately no
# privilege branch. The installer does not escalate, so a path it cannot write
# surfaces as a permission error rather than being worked around.

# Cargo and the user config must never run as root: a root-owned build
# directory is a recurring source of "permission denied" for the next ordinary
# run. Rather than re-exec through sudo to undo a privilege the caller already
# has, this installer declines to run as root at all and says how to proceed.
if [[ $EUID -eq 0 ]]; then
    echo 'Run ./installer/install.sh as your normal user, not as root.' >&2
    echo 'The default prefix needs no privileges; for a system-wide install,' >&2
    echo 'arrange write access to the prefix yourself and re-run as that user.' >&2
    exit 1
fi
[[ -n "${HOME:-}" ]] || { echo 'HOME not set' >&2; exit 1; }
export PATH="$HOME/.cargo/bin:$PATH"

# A bare ./install.sh installs for the current user. A system or custom prefix
# is always something the caller asked for by name.
if [[ "$SYSTEM_INSTALL" == true && -z "$PREFIX" ]]; then
    PREFIX=/usr/local
fi
PREFIX="${PREFIX:-$HOME/.local}"
[[ "$PREFIX" == /* && "$PREFIX" != *$'\n'* && "$PREFIX" != *$'\r'* ]] || {
    echo 'Install prefix must be an absolute, single-line path.' >&2; exit 2;
}
PREFIX="$(realpath -m -- "$PREFIX")"
cd "$APP_DIR"

BIN_DIR="$PREFIX/bin"

# The runtime binaries this installer installs, in one place.
#
# Every count in the installer is derived from this list rather than written
# out: the places that used to say "4" and the one that said "3" were already
# inconsistent with each other, and when the list shrank they made the final
# verification reject an installation it had just performed correctly.
# A literal next to a list is a second source of truth waiting to be wrong.
RUNTIME_BINS=(maverick maverickctl)
RUNTIME_BIN_COUNT="${#RUNTIME_BINS[@]}"

# The session file lives inside the prefix and nowhere else. Display managers
# generally read only system locations, so a user installation's copy is there
# for anything that does look in the prefix; installing into a location a
# display manager actually reads is the job of --xsessions-dir, which is
# explicit because it is the one case that must leave the prefix.
XS_DIR="$PREFIX/share/xsessions"

LOG_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/maverick"

# Build where the caller asked. An inherited CARGO_TARGET_DIR is used exactly
# as given, including when it already holds artifacts from an earlier build;
# the default lives in the user's cache rather than in the checkout, so merely
# running the installer never leaves a build tree in the source tree.
if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
    CARGO_TARGET_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/maverick/target"
fi
export CARGO_TARGET_DIR
[[ ! -L "$CARGO_TARGET_DIR" ]] || {
    echo "Refusing a symlinked target directory: $CARGO_TARGET_DIR" >&2
    echo 'Point CARGO_TARGET_DIR at a real directory.' >&2; exit 1;
}

INSTALL_TMP=""
STAGE_DIR=""
# Both cleanups are idempotent and are called from the exit handler, so a
# failure at any point leaves no staging directory and no temporary file
# behind. STAGE_DIR is cleared once the set has been committed.
install_cleanup() {
    if [[ -n "$INSTALL_TMP" ]]; then rm -rf -- "$INSTALL_TMP"; fi
}
stage_cleanup() {
    if [[ -n "$STAGE_DIR" && -d "$STAGE_DIR" ]]; then rm -rf -- "$STAGE_DIR"; fi
    STAGE_DIR=""
}
# One registration for the whole installer: the exit handler releases the
# staging state and puts the cursor back, the signal handler takes a running
# build down with it. Registered where that state is created, so there is no
# window in between and no second registration to disagree with the first.
trap _on_exit EXIT
trap _on_int INT TERM

# A prefix the caller cannot write is reported, not escalated around. This
# installer has no privilege to acquire and does not try to acquire one, so an
# unwritable prefix is a permission error the caller decides how to handle.
for destination in "$PREFIX" "$BIN_DIR" "$XS_DIR"; do
    parent="$destination"
    while [[ ! -e "$parent" ]]; do parent="$(dirname "$parent")"; done
    if [[ ! -w "$parent" ]]; then
        echo "cannot write to $destination (nearest existing parent: $parent)" >&2
        echo 'Choose a writable prefix, e.g. --prefix "$HOME/.local", or arrange' >&2
        echo 'write access to this one yourself. This installer does not use sudo.' >&2
        exit 1
    fi
done

# A build directory holding another user's files cannot be repaired without
# privileges this installer does not have, so it is named instead of chowned.
if [[ -d "$CARGO_TARGET_DIR" ]]; then
    foreign="$(find -P "$CARGO_TARGET_DIR" -xdev ! -uid "$(id -u)" -print -quit 2>/dev/null)" \
        || foreign="(unreadable)"
    if [[ -n "$foreign" ]]; then
        echo "cargo target directory has files owned by another user:" >&2
        echo "  $foreign" >&2
        echo "Repair or remove them, or set CARGO_TARGET_DIR to a directory you own." >&2
        exit 1
    fi
fi

# ── init: read the environment the libraries were waiting for ────────────────
# After the command line, because i18n_init needs --lang and ui_init needs
# --no-anim; before anything that prints, because everything below may.
i18n_init
ui_init
os_detect

confirm_install() {
    # The first question, and the only one whose answer can still leave every
    # file exactly as it was: nothing has been written when it is asked.
    if prompt "$(t confirm_q) $PREFIX?" "$(t confirm_hint)" yes; then
        return 0
    fi
    printf '  %s%s%s\n' "$DIM" "$(t aborted)" "$RESET"
    exit 0
}

# ── steps ───────────────────────────────────────────────────────────────────
# Each step owns one panel row and the code that fills it. They run in order
# from main() and share the panel state (PH_*, OVERALL, *_VALUE) instead of
# passing it back and forth: that state *is* the report being drawn.

step_deps() {
    # ── step 0 · dependencias ────────────────────────────────────────────────────
    _step_begin 0 "$DEPS_DETAIL"
    if [[ "$NO_BUILD" != true ]]; then
        command -v cargo >/dev/null 2>&1 || die "$(t need_cargo)"
    fi
    # The probe only earns its keep when a compile follows it. What it checks
    # and which packages install it lives with the probe, in lib/setup.sh.
    if [[ "$NO_BUILD" != true ]]; then
        check_x11_linkable
    fi
    _animate 8 "$DEPS_DETAIL"
    _nap 0.2
    _step_end 0 "$DEPS_DETAIL"
}

step_build() {
    # ── step 1 · build ───────────────────────────────────────────────────────────
    FINAL_LOG_PATH=""
    if [[ "$NO_BUILD" == true ]]; then
        _animate 64 ""
        _step_skip 1
    else
        _step_begin 1 "$(t building_detail)"
        BUILD_LOG="$(mktemp /tmp/maverick-build.XXXXXX)"

        # Progress needs a denominator. Count the real dependency tree for the
        # packages actually being compiled — the WM and the control client, the
        # two binaries the installer installs. A fixed estimate is only the last
        # resort when `cargo tree` cannot answer, because an estimate that can
        # only grow leaves the bar pinned below 100% for the whole build.
        estimated_crates="$(cargo tree --edges normal,build --prefix none \
            -p maverick -p maverickctl 2>/dev/null | sort -u | grep -c . || true)"
        if [[ -z "$estimated_crates" || "$estimated_crates" -lt 1 ]]; then
            estimated_crates=12
        fi
        total_crates=$estimated_crates

        (
            if RUSTFLAGS="-C target-cpu=native" cargo build --release \
                   -p maverick -p maverickctl >"$BUILD_LOG" 2>&1; then
                exit 0
            fi
            cargo build --release \
                -p maverick -p maverickctl >>"$BUILD_LOG" 2>&1
        ) &
        BUILD_PID=$!

        max_seen_crates=0

        if [[ $HAS_TTY -eq 1 ]]; then
            while kill -0 "$BUILD_PID" 2>/dev/null; do
                compiled="$(grep -c 'Compiling' "$BUILD_LOG" 2>/dev/null || echo 0)"
                compiled="${compiled//[^0-9]/}"
                [[ -n "$compiled" ]] || compiled=0

                if (( compiled > max_seen_crates )); then
                    max_seen_crates=$compiled
                    if (( compiled > total_crates )); then
                        total_crates=$(( compiled + 5 ))
                    fi
                fi

                _now_ms
                elapsed_s=$(( ( NOW_MS - ${PH_T0[1]} ) / 1000 ))

                if grep -q 'Finished' "$BUILD_LOG" 2>/dev/null; then
                    sub=97
                elif (( compiled > 0 )); then
                    sub=$(( compiled * 100 / total_crates ))
                else
                    sub=$(( elapsed_s * 4 ))
                    if (( sub > 50 )); then sub=50; fi
                fi
                if (( sub > 97 )); then sub=97; fi
                if (( sub < 2 )); then sub=2; fi
                target=$(( 8 + sub * 56 / 100 ))
                if (( target > 64 )); then target=64; fi
                if (( target > OVERALL )); then OVERALL=$target; fi

                crate=""
                if (( compiled > 0 )); then
                    crate="$(grep 'Compiling' "$BUILD_LOG" 2>/dev/null | tail -n1 | sed -n 's/.*Compiling \([a-zA-Z0-9_.-]*\).*/\1/p' || echo "")"
                    crate="${crate//[^a-zA-Z0-9_.-]/}"
                fi

                if [[ -z "$crate" ]]; then
                    if grep -q 'Finished' "$BUILD_LOG" 2>/dev/null; then
                        crate="$(t linking)"
                    elif (( compiled == 0 && elapsed_s > 1 )); then
                        crate="$(t cached)"
                    else
                        crate="$(t building_detail)"
                    fi
                fi

                warns="$(grep -c '^warning' "$BUILD_LOG" 2>/dev/null || echo 0)"
                warns="${warns//[^0-9]/}"
                [[ -n "$warns" ]] || warns=0
                wtag=""
                if (( warns > 0 )); then wtag=" ⚠$warns"; fi

                if (( compiled >= 2 && elapsed_s >= 1 )); then
                    eta=$(( ( total_crates - compiled ) * elapsed_s / compiled ))
                    if (( eta < 0 )); then eta=0; fi
                    LIVE_ETA="≈${eta}s"
                else
                    LIVE_ETA=""
                fi
                _render "$crate · ${compiled}/${total_crates}${wtag}"
                if [[ $ANIM -eq 1 ]]; then sleep 0.1; else sleep 0.25; fi
            done
        else
            _now_ms
            last_mark=$NOW_MS
            while kill -0 "$BUILD_PID" 2>/dev/null; do
                sleep 1
                _now_ms
                if (( NOW_MS - last_mark >= 5000 )); then
                    compiled="$(grep -c 'Compiling' "$BUILD_LOG" 2>/dev/null || echo 0)"
                    compiled="${compiled//[^0-9]/}"
                    [[ -n "$compiled" ]] || compiled=0
                    printf '  … %s %s/%s\n' "$(t step_build)" "$compiled" "$total_crates"
                    last_mark=$NOW_MS
                fi
            done
        fi
        rc=0
        wait "$BUILD_PID" || rc=$?
        BUILD_PID=""
        if [[ $rc -ne 0 ]]; then
            printf '\n'
            printf '  %s⡱⢎%s  %s%s%s\n' "$RED" "$RESET" "$BOLD" "$(t build_fail)" "$RESET" >&2
            sed 's/^/  /' "$BUILD_LOG" 2>/dev/null | tail -n 30 >&2 || true
            printf '  %slog → %s%s\n\n' "$DIM" "$BUILD_LOG" "$RESET" >&2
            exit 1
        fi

        # --keep-log copies the build log somewhere it survives the run.
        if [[ "$KEEP_LOG" == true ]]; then
            mkdir -p "$LOG_DIR" 2>/dev/null || true
            FINAL_LOG_PATH="$LOG_DIR/install.log"
            cp "$BUILD_LOG" "$FINAL_LOG_PATH" 2>/dev/null || true
        fi
        rm -f "$BUILD_LOG"

        LIVE_ETA=""
        _animate 64 "$(t build_ok)"
        _step_end 1 "$(t build_ok)"
    fi
}

step_install() {
    # ── step 2 · binarios ────────────────────────────────────────────────────────
    _step_begin 2 ""
    # Check the complete artifact set before replacing any installed binary.
    for bin in "${RUNTIME_BINS[@]}"; do
        [[ -f "$CARGO_TARGET_DIR/release/$bin" && -x "$CARGO_TARGET_DIR/release/$bin" ]] ||
            die "$(t err_missing_exe): $CARGO_TARGET_DIR/release/$bin $(t err_missing_exe_hint)"
    done
    "$CARGO_TARGET_DIR/release/maverick" --version >/dev/null 2>&1 || die "$(t verify_fail)"
    mkdir -p -- "$BIN_DIR" || die "$(t no_write): $BIN_DIR"

    # Stage the whole set before replacing anything. Committing one binary at a
    # time could leave a prefix holding a new maverick beside an old maverickctl,
    # which is a version-skewed pair that still type-checks and still misbehaves.
    # Staging first means a missing artifact, a full disk or a bad destination
    # fails while the installed prefix is still entirely the previous one.
    STAGE_DIR="$BIN_DIR/.maverick-stage.$$"
    mkdir -p -- "$STAGE_DIR" || die "$(t no_write): $STAGE_DIR"
    for bin in "${RUNTIME_BINS[@]}"; do
        install -m 0755 -- "$CARGO_TARGET_DIR/release/$bin" "$STAGE_DIR/$bin" \
            || { stage_cleanup; die "$(t err_stage_failed): $bin"; }
    done
    # Verify the staged set before any of it becomes the installed set. A
    # destination that is a directory rather than a file is caught here, while the
    # rename that would have failed is still ahead of us.
    for bin in "${RUNTIME_BINS[@]}"; do
        [[ -f "$STAGE_DIR/$bin" && -x "$STAGE_DIR/$bin" ]] \
            || { stage_cleanup; die "$(t err_staged_not_exec): $bin"; }
        if [[ -d "$BIN_DIR/$bin" && ! -L "$BIN_DIR/$bin" ]]; then
            stage_cleanup
            die "$BIN_DIR/$bin $(t err_is_dir)"
        fi
    done

    # Commit. Each move is a rename within one directory, so a running executable
    # is never truncated (ETXTBSY) and the set lands as a unit.
    n_ok=0
    for bin in "${RUNTIME_BINS[@]}"; do
        mv -fT -- "$STAGE_DIR/$bin" "$BIN_DIR/$bin" \
            || { stage_cleanup; die "$(t err_install_failed): $bin"; }
        n_ok=$(( n_ok + 1 ))
        if (( OVERALL < 78 )); then OVERALL=$(( OVERALL + 2 )); fi
        _render "$bin"
        _nap 0.05
    done
    rm -rf -- "$STAGE_DIR"
    STAGE_DIR=""
    _animate 80 "$n_ok/$RUNTIME_BIN_COUNT"
    _step_end 2 "$n_ok/$RUNTIME_BIN_COUNT · $(t installed)"
}

step_session() {
    # ── step 3 · sesión X11 ──────────────────────────────────────────────────────
    _step_begin 3 ""
    SESSION_VALUE=""
    INSTALL_TMP="$(mktemp -d /tmp/maverick-install.XXXXXX)"
    # Desktop Entry Exec quoting is not shell quoting. Escape reserved characters
    # and percent field codes so custom prefixes (including spaces) work literally.
    exec_path="$BIN_DIR/maverick"
    exec_path="${exec_path//\\/\\\\\\\\}"
    exec_path="${exec_path//\"/\\\\\"}"
    exec_path="${exec_path//\$/\\\\\$}"
    exec_path="${exec_path//\`/\\\\\`}"
    exec_path="${exec_path//%/%%}"
    printf '%s\n' \
        '[Desktop Entry]' \
        'Name=maverick' \
        'Comment=Columnar tiling WM — keyboard-driven' \
        "Exec=\"$exec_path\"" \
        'Type=Application' > "$INSTALL_TMP/maverick.desktop"
    mkdir -p -- "$XS_DIR" || die "$(t no_write): $XS_DIR"
    install -m 0644 -- "$INSTALL_TMP/maverick.desktop" "$XS_DIR/maverick.desktop" \
        || die "$(t err_session_install): $XS_DIR/maverick.desktop"
    SESSION_VALUE="$XS_DIR/maverick.desktop"

    # A display manager usually reads only a system location, so the in-prefix copy
    # alone will not appear in a session chooser. That is why this second copy is
    # opt-in: it is the one write that leaves the prefix, so it happens only when
    # the caller names the directory.
    if [[ -n "$XSESSIONS_DIR" ]]; then
        xsessions_abs="$(realpath -m -- "$XSESSIONS_DIR")"
        parent="$xsessions_abs"
        while [[ ! -e "$parent" ]]; do parent="$(dirname "$parent")"; done
        [[ -w "$parent" ]] || die "$(t no_write): $xsessions_abs (checked $parent)"
        mkdir -p -- "$xsessions_abs" || die "$(t no_write): $xsessions_abs"
        install -m 0644 -- "$INSTALL_TMP/maverick.desktop" \
            "$xsessions_abs/maverick.desktop" \
            || die "$(t err_session_install): $xsessions_abs/maverick.desktop"
        SESSION_VALUE="$xsessions_abs/maverick.desktop"
    fi
    _animate 84 "$SESSION_VALUE"
    _step_end 3 "$SESSION_VALUE"
}

step_config() {
    # ── step 4 · configuración ───────────────────────────────────────────────────
    _step_begin 4 ""
    CONFIG_VALUE=""
    case "$CONFIG_ACTION" in
        skip)
            _animate 88 "$(t config_skip)"
            _step_skip 4 "$(t config_skip)"
            ;;
        keep)
            _animate 88 "$(t config_keep)"
            CONFIG_VALUE="$CFG_FILE"
            _step_end 4 "$(t config_keep)"
            ;;
        create|overwrite)
            _animate 88 "$CFG_FILE"
            if ! mkdir -p "$CFG_DIR" 2>/dev/null; then
                die "$(t no_write): $CFG_DIR"
            fi
            wrote=0
            # The example configuration ships with the repository, one level up
            # from this directory; a standalone copy placed beside the entry
            # point is honoured too, so the sandbox and a checkout behave the
            # same way. Both are optional: the heredoc below is the floor.
            for sample in "$APP_DIR/config/config.toml" "$APP_DIR/../config/config.toml"; do
                if [[ -f "$sample" ]] && cp "$sample" "$CFG_FILE" 2>/dev/null; then
                    wrote=1
                    break
                fi
            done
            if [[ $wrote -eq 0 ]]; then
                # No [autostart] table at all: an omitted table keeps the compiled
                # defaults, whereas an empty `commands = []` is a value of the
                # wrong shape and the config loader discards it with a warning,
                # which then fails the --check-config run below.
                if cat > "$CFG_FILE" <<'TOML'
# Maverick — generated by install.sh
[general]
border_width = 2
gaps_inner = 6
gaps_outer = 6
theme = "catppuccin-mocha"

[[keybindings]]
key = "Mod4+Return"
action = "spawn:alacritty"

[[keybindings]]
key = "Mod4+p"
action = "spawn:rofi -show drun"
TOML
                then
                    wrote=1
                fi
            fi
            if [[ $wrote -eq 0 ]]; then
                die "$(t no_write): $CFG_FILE"
            fi
            cfg_ok=""
            if [[ -x "$BIN_DIR/maverick" ]] \
               && "$BIN_DIR/maverick" --check-config "$CFG_FILE" >/dev/null 2>&1; then
                cfg_ok=" ✓"
            fi
            CONFIG_VALUE="$CFG_FILE"
            _step_end 4 "${CFG_FILE}${cfg_ok}"
            ;;
    esac
}

# The probe the last step runs, kept beside them: it answers one question
# — did the binaries we just installed actually run — and dies loudly if not.
verify_installed() {
    local label="$1"; shift
    if ! "$@" >/dev/null 2>&1; then
        die "$(t verify_fail): $label"
    fi
    VERIFY_CHECKS=$(( VERIFY_CHECKS + 1 ))
}

step_verify() {
    # ── step 5 · verificación final ──────────────────────────────────────────────
    _step_begin 5 ""

    # Execute what was installed, by absolute path, and require each to succeed.
    # An executable-bit check proves only that a file exists: a stub, a truncated
    # binary or a stale copy from an earlier install all pass it. Running the real
    # commands is what distinguishes the binary just built from a file that merely
    # occupies its name.
    #
    # `maverickctl session --help` is the check that carries the most weight. The
    # session command group is the feature the installed tree exists to provide, it
    # needs no display, no running instance and no network, and an older
    # maverickctl that predates Sessions answers it as an unknown command with a
    # non-zero status. That makes it a capability probe for a stale install rather
    # than a version string that would have to be invented and kept in step.
    # Run the installed binaries, quoted as argv so a prefix containing spaces is
    # not split into words, and count the probes as they pass.
    VERIFY_CHECKS=0
    verify_installed "$BIN_DIR/maverick --version" \
        "$BIN_DIR/maverick" --version
    verify_installed "$BIN_DIR/maverickctl --help" \
        "$BIN_DIR/maverickctl" --help
    verify_installed "$BIN_DIR/maverickctl session --help" \
        "$BIN_DIR/maverickctl" session --help
    for bin in "${RUNTIME_BINS[@]}"; do
        [[ -x "$BIN_DIR/$bin" ]] || die "$(t verify_fail): $BIN_DIR/$bin"
    done

    verify_detail="$(t verify_ok)"

    _animate 100 "$(t checks_ok)"
    _step_end 5 "$VERIFY_CHECKS ✓ · $verify_detail"
    _nap 0.35
}


# ── main ─────────────────────────────────────────────────────────────────────
main() {
    check_unsupported_os

    # Column alignment degrades to bytes when wc is unavailable; say so once.
    if [[ $HAS_WC -eq 0 ]]; then
        printf '  %s⚠%s  %s\n' "$YELLOW" "$RESET" "$(t wc_missing)"
    fi

    _now_ms
    T_START=$NOW_MS
    banner
    confirm_install

    # ── config pre-check ─────────────────────────────────────────────────────────
    CFG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/maverick"
    CFG_FILE="$CFG_DIR/config.toml"
    CONFIG_ACTION="create"
    if [[ "$NO_CONFIG" == true ]]; then
        CONFIG_ACTION="skip"
    elif [[ -f "$CFG_FILE" ]]; then
        printf '  %s⠆ %s: %s%s\n' "$YELLOW" "$(t config_found)" "$CFG_FILE" "$RESET"
        if prompt "$(t config_overwrite_q)" "$(t config_overwrite_hint)" no; then
            CONFIG_ACTION="overwrite"
            printf '  %s⠤ %s%s\n' "$DIM" "$(t config_will_overwrite)" "$RESET"
        else
            CONFIG_ACTION="keep"
            printf '  %s⠤ %s%s\n' "$DIM" "$(t config_keep)" "$RESET"
        fi
        echo
    fi

    # ── pre-flight: rust antes de abrir el bloque vivo ───────────────────────────
    if [[ "$NO_BUILD" != true ]] && ! ensure_rust; then
        die "$(t need_cargo)"
    fi
    CARGO_VER=""
    if command -v cargo >/dev/null 2>&1; then
        CARGO_VER="$(cargo --version 2>/dev/null | awk '{print $2}' || true)"
    fi
    DEPS_DETAIL="$(t deps_ok)"
    if [[ -n "$CARGO_VER" ]]; then DEPS_DETAIL="cargo $CARGO_VER"; fi

    # A release build needs room; a full disk otherwise fails deep in the build.
    if [[ "$NO_BUILD" != true ]]; then
        check_disk_space "$APP_DIR"
    fi

    # ── abrir el bloque vivo ─────────────────────────────────────────────────────
    _steps_init
    echo
    if [[ $HAS_TTY -eq 1 ]]; then
        printf '\e[?25l'
        CURSOR_HIDDEN=1
    fi
    _render ""
    _nap 0.25

    # Every step, in order. The panel above them is the only thing that
    # knows they are separate: here they read as one installation.
    step_deps
    step_build
    step_install
    step_session
    step_config
    step_verify

    # ── cerrar el bloque ─────────────────────────────────────────────────────────
    printf '\n'
    if [[ $CURSOR_HIDDEN -eq 1 ]]; then
        printf '\e[?25h'
        CURSOR_HIDDEN=0
    fi
    BLOCK_OPEN=0
    _now_ms
    TOTAL_MS=$(( NOW_MS - T_START ))
    # The PATH decision belongs here: the install has succeeded, the live block is
    # closed and timed, and the summary that reports the outcome is not drawn yet.
    # It is timed out of TOTAL_MS — a prompt is the caller's seconds, not ours.
    path_setup

    # ── resumen + celebración ────────────────────────────────────────────────────
    echo
    hr
    echo
    _title_fx "$(t done_title)"
    echo
    _panel_draw
    echo
    printf '  %s%s%s\n' "$DIM" "$(t done_hint)" "$RESET"
    printf '  %s⠤%s  %s%s%s\n' "$DIM" "$RESET" "$BOLD" "$BIN_DIR/maverick" "$RESET"

    # What happened to PATH, in full. The panel shows the outcome in one line;
    # this is the part the caller has to act on, so every branch either points at
    # the files it touched or hands over the command to run instead.
    case "$PATH_RESULT" in
        in-path) ;;
        configured)
            echo
            printf '  %s⠆%s  %s: %s%s%s\n' "$GREY" "$RESET" \
                "$(t path_configured)" "$BOLD" "$(path_files_display)" "$RESET"
            printf '  %s  %s%s\n' "$DIM" "$(t path_restart)" "$RESET"
            ;;
        added)
            echo
            printf '  %s⠤%s  %s: %s%s%s\n' "$GREEN" "$RESET" \
                "$(t path_added)" "$BOLD" "$(path_files_display)" "$RESET"
            printf '  %s  %s%s\n' "$DIM" "$(t path_restart)" "$RESET"
            ;;
        refused|failed)
            echo
            if [[ "$PATH_RESULT" == "failed" && -n "$PATH_FAILED" ]]; then
                printf '  %s⡿⠿%s  %s: %s\n' "$YELLOW" "$RESET" "$PATH_FAILED" "$(t path_write_fail)"
            else
                printf '  %s⡿⠿%s  %s: %s\n' "$YELLOW" "$RESET" "$BIN_DIR" "$(t not_in_path)"
            fi
            printf '  %s  export PATH="%s:$PATH"%s\n' "$DIM" "$BIN_DIR" "$RESET"
            ;;
    esac

    # Report where the retained log went, if one was kept.
    if [[ -n "$FINAL_LOG_PATH" && -f "$FINAL_LOG_PATH" ]]; then
        echo
        printf '  %s📋%s  %s: %s%s%s\n' "$DIM" "$RESET" "$(t log_saved)" "$GREY" "$FINAL_LOG_PATH" "$RESET"
    fi

    echo
    printf '  %s%s:%s %s%s%s\n' "$GREY" "$(t tip)" "$RESET" "$DIM" "$(t tip_text)" "$RESET"
    hr
    echo
    _shimmer "⣦⠞  Maverick — $(t ready_line)"
}

main "$@"
