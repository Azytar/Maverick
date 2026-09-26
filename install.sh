#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  MAVERICK — installer
#  columnar tiling WM · bilingual (EN/ES)
#
#  Installs the release binaries into a prefix. The default prefix is
#  $HOME/.local, so a normal installation needs no privileges. A system or
#  custom prefix is always explicit, and the installer never writes outside
#  the prefix it was given and never escalates privileges on its own.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

# ── cli ──────────────────────────────────────────────────────────────────────
ORIGINAL_ARGS=("$@")
LANG_CHOICE="auto"
PREFIX="${PREFIX:-}"
SYSTEM_INSTALL=false
XSESSIONS_DIR=""
YES=false
NO_CONFIG=false
NO_BUILD=false
NO_ANIM="${MAVERICK_NO_ANIM:-0}"
WITH_COMPOSITOR=""
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
        --no-anim)   NO_ANIM=1; shift ;;
        --keep-log)  KEEP_LOG=true; shift ;;
        --with-compositor)   WITH_COMPOSITOR="yes"; shift ;;
        --without-compositor|--no-compositor) WITH_COMPOSITOR="no"; shift ;;
        --no-default-features) WITH_COMPOSITOR="no"; shift ;;
        -h|--help)
            cat <<'HELP'
Usage: ./install.sh [options]

Installs maverick and maverickctl into a prefix. The default prefix is
$HOME/.local and needs no privileges. Nothing is ever written outside the
selected prefix, and the installer never invokes sudo.

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
  --no-anim        Disable all animations (or export MAVERICK_NO_ANIM=1)
  --keep-log       Keep build log even on success (saved to share/maverick/)
  --with-compositor         Build WITH compositor (experimental)
  --without-compositor      Build WITHOUT compositor — pure X11 [ideal, default]
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

Examples:
  ./install.sh
  ./install.sh --system
  ./install.sh --prefix /opt/maverick
  ./install.sh --prefix ~/.local --lang es
  ./install.sh --yes --no-anim --keep-log
HELP
            exit 0 ;;
        *) echo "unknown option: $1" >&2; echo "try: ./install.sh --help" >&2; exit 2 ;;
    esac
done

APP_DIR="$(cd "$(dirname "$0")" && pwd -P)"

# Cargo and the user config must never run as root: a root-owned build
# directory is a recurring source of "permission denied" for the next ordinary
# run. Rather than re-exec through sudo to undo a privilege the caller already
# has, this installer declines to run as root at all and says how to proceed.
if [[ $EUID -eq 0 ]]; then
    echo 'Run ./install.sh as your normal user, not as root.' >&2
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
trap install_cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

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

# Every filesystem change goes through here. There is deliberately no privilege
# branch: the installer does not escalate, so a path it cannot write surfaces
# as a permission error rather than being worked around.
install_command() {
    "$@"
}

# ── language ─────────────────────────────────────────────────────────────────
detect_lang() {
    local src="${LC_ALL:-}${LC_MESSAGES:-}${LANG:-} $1"
    src="$(printf '%s' "$src" | command tr '[:upper:]' '[:lower:]')"
    if [[ "$src" == *"es"* ]]; then printf 'es'; else printf 'en'; fi
    return 0
}

if [[ "$LANG_CHOICE" == "auto" ]]; then
    LANG_ID="$(detect_lang "")"
else
    case "$LANG_CHOICE" in
        es|es_*|spanish) LANG_ID="es" ;;
        *) LANG_ID="en" ;;
    esac
fi

# ── os / distro ──────────────────────────────────────────────────────────────
OS_UNAME="$(uname -s 2>/dev/null || echo unknown)"
OS_ID="linux"; OS_PRETTY="Linux"
DISTRO_ID="unknown"; DISTRO_PRETTY="Linux"
case "$OS_UNAME" in
    Linux*)  OS_ID="linux"; OS_PRETTY="Linux" ;;
    Darwin*) OS_ID="macos"; OS_PRETTY="macOS" ;;
    MINGW*|MSYS*|CYGWIN*|Windows_NT*) OS_ID="windows"; OS_PRETTY="Windows" ;;
    FreeBSD*|OpenBSD*) OS_ID="bsd"; OS_PRETTY="$OS_UNAME" ;;
    *) OS_ID="unknown"; OS_PRETTY="$OS_UNAME" ;;
esac
if [[ "$OS_ID" == "linux" && -f /etc/os-release ]]; then
    # shellcheck disable=SC1091
    . /etc/os-release 2>/dev/null || true
    DISTRO_ID="${ID:-unknown}"
    DISTRO_PRETTY="${PRETTY_NAME:-${NAME:-$DISTRO_ID}}"
    if grep -qi microsoft /proc/version 2>/dev/null; then
        DISTRO_PRETTY="$DISTRO_PRETTY (WSL2)"
    fi
fi

# ── i18n ─────────────────────────────────────────────────────────────────────
t() {
    local k="$1"
    if [[ "$LANG_ID" == "es" ]]; then
        case "$k" in
            banner_sub)      echo "gestor de ventanas columnar · tiling WM" ;;
            detect_lang)     echo "idioma detectado" ;;
            os_detected)     echo "sistema detectado" ;;
            distro_detected) echo "distro" ;;
            dest)            echo "destino" ;;
            confirm_q)       echo "¿Instalar Maverick en" ;;
            confirm_hint)    echo "[S/n]" ;;
            aborted)         echo "cancelado." ;;
            need_cargo)      echo "cargo no está en el PATH — instala rustup desde https://rustup.rs" ;;
            need_rust)       echo "Rust no encontrado — se necesita cargo ≥ 1.82" ;;
            install_rust_q)  echo "¿Instalar Rust ahora?" ;;
            install_rust_hint) echo "[s/N]" ;;
            installing_rust) echo "instalando Rust…" ;;
            rust_ok)         echo "Rust instalado" ;;
            rust_fail)       echo "no se pudo instalar Rust automáticamente" ;;
            unsupported)     echo "sistema no soportado" ;;
            use_linux)       echo "Maverick es un window manager X11 solo para Linux. En Windows usa WSL2 con una distro Linux." ;;
            x11_only)        echo "requiere Linux con X11 (X.Org / XLibre)" ;;
            phase_deps)      echo "Verificando dependencias" ;;
            phase_build)     echo "Compilando" ;;
            phase_install)   echo "Instalando binarios" ;;
            phase_session)   echo "Sesión X11" ;;
            phase_config)    echo "Configuración" ;;
            phase_final)     echo "Verificación final" ;;
            deps_ok)         echo "todo en orden" ;;
            building_detail) echo "puede tardar un momento" ;;
            build_ok)        echo "compilación completada" ;;
            build_fail)      echo "falló la compilación" ;;
            cached)          echo "caché ✓" ;;
            linking)         echo "enlazando…" ;;
            skipped_word)    echo "omitido" ;;
            no_write)        echo "sin permiso de escritura en" ;;
            config_found)    echo "configuración existente en" ;;
            config_overwrite_q) echo "¿Sobrescribir?" ;;
            config_overwrite_hint) echo "[s/N]" ;;
            config_keep)     echo "se conservará la existente" ;;
            config_will_overwrite) echo "se sobrescribirá al instalar" ;;
            config_skip)     echo "configuración omitida" ;;
            compositor_q)    echo "¿Compilar con compositor? (experimental)" ;;
            compositor_hint) echo "[s/N] — ideal: sin compositor" ;;
            compositor_yes)  echo "con compositor (experimental)" ;;
            compositor_no)   echo "sin compositor — puro X11 [ideal]" ;;
            comp_no_short)   echo "X11 puro · sin compositor" ;;
            comp_yes_short)  echo "con compositor (experimental)" ;;
            done_title)      echo "¡Listo! Maverick instalado" ;;
            done_hint)       echo "Selecciona «maverick» en tu gestor de sesión, o ejecuta:" ;;
            tip)             echo "consejo" ;;
            tip_text)        echo "maverick --check-config valida tu config sin iniciar el WM" ;;
            s_binaries)      echo "binarios" ;;
            s_session)       echo "sesión" ;;
            s_config)        echo "config" ;;
            s_mode)          echo "modo" ;;
            s_time)          echo "tiempo" ;;
            installed)       echo "instalado" ;;
            not_in_path)     echo "no está en PATH — añade:" ;;
            checks_ok)       echo "todos los sistemas listos" ;;
            ready_line)      echo "listo para el despegue" ;;
            disk_space_warn) echo "espacio en disco bajo (<2GB libres)" ;;
            disk_space_ok)   echo "espacio suficiente" ;;
            verify_ok)       echo "binario funcional" ;;
            verify_fail)     echo "el binario no responde correctamente" ;;
            log_saved)       echo "log guardado en" ;;
            wc_missing)      echo "wc no encontrado — alineación puede ser imprecisa" ;;
            *) echo "$k" ;;
        esac
    else
        case "$k" in
            banner_sub)      echo "columnar tiling window manager" ;;
            detect_lang)     echo "language detected" ;;
            os_detected)     echo "system detected" ;;
            distro_detected) echo "distro" ;;
            dest)            echo "prefix" ;;
            confirm_q)       echo "Install Maverick to" ;;
            confirm_hint)    echo "[Y/n]" ;;
            aborted)         echo "aborted." ;;
            need_cargo)      echo "cargo not found in PATH — install rustup from https://rustup.rs" ;;
            need_rust)       echo "Rust not found — cargo ≥ 1.82 required" ;;
            install_rust_q)  echo "Install Rust now?" ;;
            install_rust_hint) echo "[y/N]" ;;
            installing_rust) echo "installing Rust…" ;;
            rust_ok)         echo "Rust installed" ;;
            rust_fail)       echo "automatic Rust install failed" ;;
            unsupported)     echo "unsupported system" ;;
            use_linux)       echo "Maverick is an X11 window manager for Linux only. On Windows, use WSL2 with a Linux distro." ;;
            x11_only)        echo "requires Linux with X11 (X.Org / XLibre)" ;;
            phase_deps)      echo "Checking dependencies" ;;
            phase_build)     echo "Building" ;;
            phase_install)   echo "Installing binaries" ;;
            phase_session)   echo "X11 session" ;;
            phase_config)    echo "Configuration" ;;
            phase_final)     echo "Final verification" ;;
            deps_ok)         echo "all good" ;;
            building_detail) echo "this may take a moment" ;;
            build_ok)        echo "build complete" ;;
            build_fail)      echo "build failed" ;;
            cached)          echo "cached ✓" ;;
            linking)         echo "linking…" ;;
            skipped_word)    echo "skipped" ;;
            no_write)        echo "cannot write to" ;;
            config_found)    echo "existing config found at" ;;
            config_overwrite_q) echo "Overwrite?" ;;
            config_overwrite_hint) echo "[y/N]" ;;
            config_keep)     echo "will keep existing" ;;
            config_will_overwrite) echo "will overwrite on install" ;;
            config_skip)     echo "config skipped" ;;
            compositor_q)    echo "Build with compositor? (experimental)" ;;
            compositor_hint) echo "[y/N] — ideal: without" ;;
            compositor_yes)  echo "with compositor (experimental)" ;;
            compositor_no)   echo "without compositor — pure X11 [ideal]" ;;
            comp_no_short)   echo "pure X11 · no compositor" ;;
            comp_yes_short)  echo "with compositor (experimental)" ;;
            done_title)      echo "Done — Maverick installed" ;;
            done_hint)       echo "Select 'maverick' in your display manager, or run:" ;;
            tip)             echo "tip" ;;
            tip_text)        echo "maverick --check-config validates your config without starting the WM" ;;
            s_binaries)      echo "binaries" ;;
            s_session)       echo "session" ;;
            s_config)        echo "config" ;;
            s_mode)          echo "mode" ;;
            s_time)          echo "time" ;;
            installed)       echo "installed" ;;
            not_in_path)     echo "not in PATH — add:" ;;
            checks_ok)       echo "all systems go" ;;
            ready_line)      echo "cleared for takeoff" ;;
            disk_space_warn) echo "low disk space (<2GB free)" ;;
            disk_space_ok)   echo "disk space OK" ;;
            verify_ok)       echo "binary functional" ;;
            verify_fail)     echo "binary does not respond correctly" ;;
            log_saved)       echo "log saved to" ;;
            wc_missing)      echo "wc not found — alignment may be imprecise" ;;
            *) echo "$k" ;;
        esac
    fi
    return 0
}

# ── visuals ──────────────────────────────────────────────────────────────────
if [[ -t 1 && -z "${NO_COLOR:-}" && "${TERM:-}" != "dumb" ]]; then
    HAS_TTY=1
else
    HAS_TTY=0
fi
ANIM=$HAS_TTY
if [[ "$NO_ANIM" == "1" || "$NO_ANIM" == "true" || "$NO_ANIM" == "yes" ]]; then
    ANIM=0
fi

TRUECOLOR=1
if [[ $HAS_TTY -eq 1 && -z "${COLORTERM:-}" && "${TERM:-}" == *256color* ]]; then
    TRUECOLOR=0
fi

if [[ $HAS_TTY -eq 1 ]]; then
    DIM=$'\e[2m'; BOLD=$'\e[1m'; RESET=$'\e[0m'
    if [[ $TRUECOLOR -eq 1 ]]; then
        C0=$'\e[38;2;232;93;4m'; C1=$'\e[38;2;255;154;0m'; C2=$'\e[38;2;228;213;178m'
        ACCENT=$'\e[38;2;232;93;4m'
        GREEN=$'\e[38;2;143;181;140m'; CYAN=$'\e[38;2;122;168;165m'
        YELLOW=$'\e[38;2;229;192;123m'; RED=$'\e[38;2;199;91;91m'; GREY=$'\e[38;2;122;129;153m'
    else
        C0=$'\e[38;5;202m'; C1=$'\e[38;5;214m'; C2=$'\e[38;5;222m'
        ACCENT=$'\e[38;5;202m'
        GREEN=$'\e[38;5;114m'; CYAN=$'\e[38;5;73m'
        YELLOW=$'\e[38;5;179m'; RED=$'\e[38;5;167m'; GREY=$'\e[38;5;245m'
    fi
else
    C0=""; C1=""; C2=""; ACCENT=""; DIM=""; BOLD=""; RESET=""
    GREEN=""; CYAN=""; YELLOW=""; RED=""; GREY=""
fi

COLS="$(tput cols 2>/dev/null || echo 80)"
[[ "$COLS" =~ ^[0-9]+$ ]] || COLS=80
(( COLS < 40 )) && COLS=40

SEP_W=60
if (( COLS < 62 )); then SEP_W=$(( COLS - 2 )); fi
SEP=""
printf -v SEP '%*s' "$SEP_W" ''; SEP="${SEP// /─}"

BAR_W=30
if (( COLS < 64 )); then BAR_W=16; fi
PANEL_W=48

# Pad by display width; falls back to a byte count when wc is unavailable.
_SPACES="                                                                                                                            "
HAS_WC=1
if ! command -v wc >/dev/null 2>&1; then
    HAS_WC=0
fi

_pad() {
    local str="$1"
    local width="$2"
    local n
    if [[ $HAS_WC -eq 1 ]]; then
        n=$(printf '%s' "$str" | wc -m 2>/dev/null)
        n="${n// /}"
    fi
    if [[ -z "$n" || ! "$n" =~ ^[0-9]+$ ]]; then
        n="${#str}"
    fi
    local spaces=$(( width - n ))
    if (( spaces > 0 )); then
        if (( spaces > ${#_SPACES} )); then spaces=${#_SPACES}; fi
        printf '%s%s' "$str" "${_SPACES:0:spaces}"
    else
        printf '%s' "$str"
    fi
}

# ── time — locale-safe ───────────────────────────────────────────────────────
NOW_MS=0
_now_ms() {
    local us="${EPOCHREALTIME:-}"
    us="${us//[!0-9]/}"
    if [[ "$us" =~ ^[0-9]{16}$ ]]; then
        NOW_MS=$(( us / 1000 ))
    else
        local s
        s="$(date +%s 2>/dev/null || printf 0)"
        s="${s//[!0-9]/}"
        [[ "$s" =~ ^[0-9]+$ ]] || s=0
        NOW_MS=$(( s * 1000 ))
    fi
    return 0
}

_fmt_ms() {
    local ms="${1:-0}"
    if ! [[ "$ms" =~ ^-?[0-9]+$ ]]; then ms=0; fi
    if (( ms < 0 )); then ms=0; fi
    printf '%d.%ds' "$(( ms / 1000 ))" "$(( (ms % 1000) / 100 ))"
    return 0
}

# ── tiny utils ───────────────────────────────────────────────────────────────
_cv() {
    if [[ $HAS_TTY -ne 1 ]]; then printf -v "$1" '%s' ''; return 0; fi
    if [[ $TRUECOLOR -eq 1 ]]; then
        printf -v "$1" '\e[38;2;%d;%d;%dm' "$2" "$3" "$4"
    else
        printf -v "$1" '\e[38;5;%dm' "$(( 16 + 36*( $2*6/256 ) + 6*( $3*6/256 ) + ( $4*6/256 ) ))"
    fi
    return 0
}

_grad3() {
    local tt=$2 u r g b
    if (( tt > 100 )); then tt=100; fi
    if (( tt < 0 )); then tt=0; fi
    if (( tt < 50 )); then
        u=$(( tt * 2 ))
        r=$(( 232 + 23*u/100 )); g=$(( 93 + 61*u/100 )); b=$(( 4*(100-u)/100 ))
    else
        u=$(( (tt-50) * 2 ))
        r=$(( 255 - 27*u/100 )); g=$(( 154 + 59*u/100 )); b=$(( 178*u/100 ))
    fi
    _cv "$1" "$r" "$g" "$b"
    return 0
}

_nap() { if [[ $ANIM -eq 1 ]]; then sleep "$1"; fi; return 0; }
_eoln() { if [[ $HAS_TTY -eq 1 ]]; then printf '\e[K\n'; else printf '\n'; fi; return 0; }

hr() {
    local w=$COLS
    if (( w > 72 )); then w=72; fi
    local line; printf -v line '%*s' "$w" ''; line="${line// /─}"
    printf '  %s%s%s\n' "$GREY" "$line" "$RESET"
    return 0
}

# ── cursor / señales ─────────────────────────────────────────────────────────
BLOCK_OPEN=0
CURSOR_HIDDEN=0
BUILD_PID=""

_on_exit() {
    stage_cleanup
    install_cleanup
    if [[ ${CURSOR_HIDDEN:-0} -eq 1 ]]; then
        printf '\e[?25h' >&2 || true
    fi
    return 0
}
_on_int() {
    if [[ -n "${BUILD_PID:-}" ]]; then
        kill "$BUILD_PID" 2>/dev/null || true
    fi
    printf '\n' >&2 || true
    exit 130
}
trap _on_exit EXIT
trap _on_int INT TERM

die() {
    if [[ $BLOCK_OPEN -eq 1 ]]; then printf '\n'; fi
    printf '  %s⡱⢎%s %s%s%s\n' "$RED" "$RESET" "$BOLD" "$*" "$RESET" >&2
    exit 1
}

# ── banner ───────────────────────────────────────────────────────────────────
BANNER_ART=(
'  ███╗   ███╗  █████╗  ██╗   ██╗ ███████╗ ██████╗  ██╗  ██████╗ ██╗  ██╗    '
'  ████╗ ████║ ██╔══██╗ ██║   ██║ ██╔════╝ ██╔══██╗ ██║ ██╔════╝ ██║ ██╔╝    '
'  ██╔████╔██║ ███████║ ██║   ██║ █████╗   ██████╔╝ ██║ ██║      █████╔╝     '
'  ██║╚██╔╝██║ ██╔══██║ ╚██╗ ██╔╝ ██╔══╝   ██╔══██╗ ██║ ██║      ██╔═██╗     '
'  ██║ ╚═╝ ██║ ██║  ██║  ╚████╔╝  ███████╗ ██║  ██║ ██║ ╚██████╗ ██║  ██╗    '
'  ╚═╝     ╚═╝ ╚═╝  ╚═╝   ╚═══╝   ╚══════╝ ╚═╝  ╚═╝ ╚═╝  ╚═════╝ ╚═╝  ╚═╝    '
)

_reveal_line() {
    local line="$1" color="$2" len=${#1}
    if [[ $ANIM -eq 1 ]]; then
        local s n steps=5
        for ((s=1; s<=steps; s++)); do
            n=$(( len * s / steps ))
            printf '\r  %s%s%s\e[K' "$color" "${line:0:n}" "$RESET"
            sleep 0.022
        done
    fi
    printf '\r  %s%s%s\e[K\n' "$color" "$line" "$RESET"
    return 0
}

_gradient_print() {
    local text="$1" len=${#1} out="" i cv
    for ((i=0; i<len; i++)); do
        local ch="${text:i:1}"
        if [[ "$ch" == " " ]]; then out+=" "; continue; fi
        _grad3 cv "$(( i * 100 / (len > 1 ? len-1 : 1) ))"
        out+="$cv$ch"
    done
    printf '  %s%s' "$out" "$RESET"
    return 0
}

_info_row() {
    printf '  %s⠆%s  %s%s%s  %s\n' "$GREY" "$RESET" "$DIM" "$1" "$RESET" "$2"
    _nap 0.06
    return 0
}

banner() {
    if [[ $HAS_TTY -eq 1 ]]; then
        clear 2>/dev/null || printf '\033[2J\033[H' || true
    fi
    echo
    echo
    if [[ $HAS_TTY -eq 1 && $COLS -ge 70 ]]; then
        local flat=("$C0" "$C0" "$C1" "$C1" "$C2" "$C2")
        local i
        if [[ $ANIM -eq 1 ]]; then
            for i in "${!BANNER_ART[@]}"; do
                _reveal_line "${BANNER_ART[$i]}" "${flat[$i]}"
            done
            printf '\e[6A'
        fi
        for i in "${!BANNER_ART[@]}"; do
            _gradient_print "${BANNER_ART[$i]}"
            printf '\e[K\n'
            if [[ $ANIM -eq 1 ]]; then sleep 0.045; fi
        done
    else
        echo "  MAVERICK"
    fi
    local sub; sub="$(t banner_sub)"
    if [[ $ANIM -eq 1 ]]; then
        printf '  '
        local j
        for ((j=0; j<${#sub}; j++)); do
            printf '%s%s%s' "$DIM" "${sub:j:1}" "$RESET"
            sleep 0.01
        done
        printf '\n'
    else
        printf '  %s%s%s\n' "$DIM" "$sub" "$RESET"
    fi
    hr
    _info_row "$(t detect_lang)"   "$GREY$LANG_ID$RESET"
    _info_row "$(t os_detected)"   "$BOLD$OS_PRETTY$RESET"
    if [[ "$OS_ID" == "linux" ]]; then
        _info_row "$(t distro_detected)" "$GREY$DISTRO_PRETTY$RESET"
    fi
    _info_row "$(t dest)" "$BOLD$BIN_DIR$RESET"
    hr
    echo
    return 0
}

check_unsupported_os() {
    if [[ "$OS_ID" == "windows" || "$OS_ID" == "macos" ]]; then
        printf '\n  %s⡱⢎%s  %s: %s\n' "$RED" "$RESET" "$(t unsupported)" "$OS_PRETTY"
        if [[ "$OS_ID" == "windows" ]]; then
            printf '  %s%s%s\n\n' "$DIM" "$(t use_linux)" "$RESET"
        else
            printf '  %s%s%s\n\n' "$DIM" "$(t x11_only)" "$RESET"
        fi
        exit 1
    fi
    return 0
}

# ── prompts ──────────────────────────────────────────────────────────────────
ask() {
    printf '  %s%s%s %s%s%s ' "$BOLD" "$1" "$RESET" "$DIM" "$2" "$RESET"
    REPLY=""
    read -r REPLY || REPLY=""
    REPLY="$(printf '%s' "$REPLY" | command tr '[:upper:]' '[:lower:]')"
    return 0
}

is_yes() {
    local ans="${REPLY:-}" d="${1:-yes}"
    if [[ -z "$ans" ]]; then ans="$d"; fi
    case "$ans" in
        y|yes|s|si|sí) return 0 ;;
        *) return 1 ;;
    esac
}

confirm() {
    if [[ "$YES" == true ]]; then return 0; fi
    if [[ ! -t 0 ]]; then return 0; fi
    ask "$(t confirm_q) $PREFIX?" "$(t confirm_hint)"
    if is_yes yes; then echo; return 0; fi
    echo
    printf '  %s%s%s\n' "$DIM" "$(t aborted)" "$RESET"
    exit 0
}

# ── spinners ─────────────────────────────────────────────────────────────────
SPIN_DOTS=('⠋' '⠙' '⠹' '⠸' '⠼' '⠴' '⠦' '⠧' '⠇' '⠏')
SPIN_ORBIT=('⣾' '⣽' '⣻' '⢿' '⡿' '⣟' '⣯' '⣷')
SPIN_PULSE=('●' '◐' '○' '◐')
SPIN_IDX=0
SPIN_CHAR="⠋"

_spin_pick() {
    local f=$(( $1 % 3 ))
    case $f in
        0) SPIN_CHAR="${SPIN_DOTS[$(( SPIN_IDX % ${#SPIN_DOTS[@]} ))]}" ;;
        1) SPIN_CHAR="${SPIN_ORBIT[$(( SPIN_IDX % ${#SPIN_ORBIT[@]} ))]}" ;;
        2) SPIN_CHAR="${SPIN_PULSE[$(( SPIN_IDX % ${#SPIN_PULSE[@]} ))]}" ;;
    esac
    return 0
}

# ── roadmap + barra viva ─────────────────────────────────────────────────────
N_PHASES=6
PH_LABEL=(); PH_STATE=(); PH_DETAIL=(); PH_TIME=(); PH_T0=()
OVERALL=0
PH_FLASH=-1
LIVE_ETA=""

_phases_init() {
    PH_LABEL=( "$(t phase_deps)" "$(t phase_build)" "$(t phase_install)" \
               "$(t phase_session)" "$(t phase_config)" "$(t phase_final)" )
    PH_STATE=(0 0 0 0 0 0)
    PH_DETAIL=("" "" "" "" "" "")
    PH_TIME=("" "" "" "" "" "")
    PH_T0=(0 0 0 0 0 0)
    return 0
}

BAR_STR=""
_bar_str() {
    local pct=$1 i cv out="" total full frac last
    if (( pct > 100 )); then pct=100; fi
    if (( pct < 0 )); then pct=0; fi
    total=$(( pct * BAR_W * 8 / 100 ))
    full=$(( total / 8 ))
    frac=$(( total % 8 ))
    last=$(( full > 0 ? full - 1 : 0 ))
    for ((i=0; i<full; i++)); do
        if (( pct >= 100 )); then
            local t2=$(( i * 100 / (BAR_W - 1) ))
            _cv cv "$(( 143 + 85*t2/100 ))" "$(( 181 + 32*t2/100 ))" "$(( 140 + 38*t2/100 ))"
        elif (( i == last && full > 1 )); then
            _cv cv 255 240 200
        elif (( i == last - 1 && full > 2 )); then
            _cv cv 255 205 120
        else
            _grad3 cv "$(( i * 100 / (BAR_W - 1) ))"
        fi
        out+="$cv█"
    done
    if (( frac > 0 && full < BAR_W )); then
        _grad3 cv "$(( full * 100 / (BAR_W - 1) ))"
        local fch
        case $frac in
            1) fch="▏" ;; 2) fch="▎" ;; 3) fch="▍" ;; 4) fch="▌" ;;
            5) fch="▋" ;; 6) fch="▊" ;; 7) fch="▉" ;; *) fch="█" ;;
        esac
        out+="$cv$fch"
    fi
    local empt=$(( BAR_W - full - (frac > 0 ? 1 : 0) ))
    local es
    printf -v es '%*s' "$empt" ''
    out+="${GREY}${es// /·}${RESET}"
    BAR_STR="$out"
    return 0
}

_render() {
    [[ $HAS_TTY -eq 1 ]] || return 0
    _now_ms
    local now=$NOW_MS
    local live="${1:-}"
    local label_w=24 detail_w=26
    if (( COLS < 68 )); then
        label_w=17
        detail_w=$(( COLS - 40 ))
    fi
    if (( detail_w > 26 )); then detail_w=26; fi
    if (( detail_w < 6 )); then detail_w=6; fi
    local i
    if [[ $BLOCK_OPEN -eq 1 ]]; then
        printf '\e[%dA\r' "$(( N_PHASES + 1 ))"
    fi
    BLOCK_OPEN=1
    for ((i=0; i<N_PHASES; i++)); do
        local mark="·" mcol="$GREY" lcol="$DIM" dcol="$DIM" tcol="$GREY"
        local det="${PH_DETAIL[$i]}" lbl="${PH_LABEL[$i]}" tstr=""
        case "${PH_STATE[$i]}" in
            1)
                _spin_pick "$i"
                mark="$SPIN_CHAR"; mcol="$ACCENT"; dcol="$RESET"
                if [[ -n "$live" ]]; then det="$live"; fi
                if (( ${PH_T0[$i]} > 0 )); then
                    local el=$(( now - ${PH_T0[$i]} ))
                    if (( el < 0 )); then el=0; fi
                    tstr="$(_fmt_ms "$el")"
                fi
                ;;
            2) mark="✓"; mcol="$GREEN"; lcol="$RESET"; tstr="${PH_TIME[$i]}" ;;
            3) mark="○"; mcol="$GREY" ;;
        esac
        if [[ $i -eq $PH_FLASH ]]; then mark="⠿"; mcol="$GREEN"; fi
        if (( ${#lbl} > label_w )); then lbl="${lbl:0:$(( label_w - 1 ))}…"; fi
        if (( ${#det} > detail_w )); then det="${det:0:$(( detail_w - 1 ))}…"; fi
        
        local lbl_pad det_pad
        lbl_pad="$(_pad "$lbl" "$label_w")"
        det_pad="$(_pad "$det" "$detail_w")"
        
        printf '  %s%s%s  %s%s%s  %s%s%s  %s%6s%s\e[K\n' \
            "$mcol" "$mark" "$RESET" \
            "$lcol" "$lbl_pad" "$RESET" \
            "$dcol" "$det_pad" "$RESET" \
            "$tcol" "$tstr" "$RESET"
    done
    printf '  %s%s%s\e[K\n' "$GREY" "$SEP" "$RESET"
    _bar_str "$OVERALL"
    local etastr=""
    if [[ -n "$LIVE_ETA" ]]; then etastr="  $DIM$LIVE_ETA$RESET"; fi
    printf '  %s %s%3d%%%s%s\e[K' "$BAR_STR" "$DIM" "$OVERALL" "$RESET" "$etastr"
    SPIN_IDX=$(( SPIN_IDX + 1 ))
    return 0
}

_phase_begin() {
    PH_STATE[$1]=1
    PH_DETAIL[$1]="${2:-}"
    _now_ms
    PH_T0[$1]=$NOW_MS
    _render "${2:-}"
    return 0
}

_phase_end() {
    local i=$1
    PH_STATE[$i]=2
    PH_DETAIL[$i]="${2:-}"
    _now_ms
    PH_TIME[$i]="$(_fmt_ms $(( NOW_MS - ${PH_T0[$i]} )))"
    LIVE_ETA=""
    if [[ $HAS_TTY -ne 1 ]]; then
        printf '  ✓ %s — %s (%s)\n' "${PH_LABEL[$i]}" "${PH_DETAIL[$i]}" "${PH_TIME[$i]}"
        return 0
    fi
    if [[ $ANIM -eq 1 ]]; then
        PH_FLASH=$i
        _render ""
        sleep 0.05
        PH_FLASH=-1
    fi
    _render ""
    return 0
}

_phase_skip() {
    PH_STATE[$1]=3
    PH_DETAIL[$1]="${2:-$(t skipped_word)}"
    if [[ $HAS_TTY -ne 1 ]]; then
        printf '  ○ %s — %s\n' "${PH_LABEL[$1]}" "${PH_DETAIL[$1]}"
        return 0
    fi
    _render ""
    return 0
}

_animate() {
    local target=$1 live="${2:-}"
    if [[ $ANIM -ne 1 ]]; then
        OVERALL=$target
        _render "$live"
        return 0
    fi
    local guard=0
    while (( OVERALL < target && guard < 200 )); do
        local d=$(( target - OVERALL )) step=1
        if (( d >= 48 )); then step=6
        elif (( d >= 24 )); then step=3
        fi
        OVERALL=$(( OVERALL + step ))
        if (( OVERALL > target )); then OVERALL=$target; fi
        _render "$live"
        sleep 0.03
        guard=$(( guard + 1 ))
    done
    return 0
}

# ── finale fx ────────────────────────────────────────────────────────────────
_shimmer() {
    local text="$1" len=${#1}
    if [[ $ANIM -ne 1 ]]; then
        _gradient_print "$text"; _eoln
        return 0
    fi
    local tt i d cv out
    for ((tt=0; tt <= len + 10; tt++)); do
        out=""
        for ((i=0; i<len; i++)); do
            d=$(( tt - i )); if (( d < 0 )); then d=$(( -d )); fi
            local ch="${text:i:1}"
            if (( d == 0 )); then _cv cv 255 255 255
            elif (( d == 1 )); then _cv cv 255 230 170
            elif (( d <= 4 )); then _cv cv 252 190 110
            else _cv cv 168 120 72; fi
            out+="$cv$ch"
        done
        printf '\r  %s%s\e[K' "$out" "$RESET"
        sleep 0.045
    done
    _gradient_print "$text"; _eoln
    return 0
}

_title_fx() {
    local text="$1" len=${#1}
    if [[ $ANIM -ne 1 ]]; then
        _gradient_print "$text"; _eoln
        return 0
    fi
    printf '  '
    local i cv
    for ((i=0; i<len; i++)); do
        _grad3 cv "$(( i * 100 / (len > 1 ? len-1 : 1) ))"
        printf '%s%s' "$cv" "${text:i:1}"
        sleep 0.028
    done
    local sp
    for sp in 4 13 7 20 26; do
        printf '\e7\e[1A\r\e[%dC%s✦%s\e8' "$sp" "$C1" "$RESET"
        sleep 0.09
    done
    sleep 0.18
    printf '\e[1A\r\e[K\e[1B\r\e[K'
    _shimmer "$text"
    return 0
}

# ── panel resumen ────────────────────────────────────────────────────────────
_panel_row() {
    local label="$1" value="$2" vc="${3:-$C2}"
    if (( ${#label} > 14 )); then label="${label:0:13}…"; fi
    local label_pad
    label_pad="$(_pad "$label" 14)"
    local room=$(( PANEL_W - 17 ))
    if (( ${#value} > room )); then value="${value:0:$(( room - 1 ))}…"; fi
    local value_pad
    value_pad="$(_pad "$value" "$room")"
    printf '  %s│%s  %s%s%s %s%s%s%s│%s\n' \
        "$GREY" "$RESET" \
        "$DIM" "$label_pad" "$RESET" \
        "$vc" "$value_pad" "$RESET" \
        "$GREY" "$RESET"
    return 0
}

_panel_draw() {
    local h t
    printf -v h '%*s' "$PANEL_W" ''; h="${h// /─}"
    t="$(t done_title)"
    if (( ${#t} > 43 )); then t="${t:0:42}…"; fi
    local t_pad
    t_pad="$(_pad "$t" 43)"
    printf '  %s╭%s╮%s\n' "$GREY" "$h" "$RESET"
    printf '  %s│%s  %s✓%s  %s%s%s%s│%s\n' \
        "$GREY" "$RESET" "$GREEN" "$RESET" "$BOLD" "$t_pad" "$RESET" "$GREY" "$RESET"
    printf '  %s├%s┤%s\n' "$GREY" "$h" "$RESET"
    _panel_row "$(t s_binaries)" "$BIN_DIR · $RUNTIME_BIN_COUNT"
    _nap 0.05
    _panel_row "$(t s_session)"  "${SESSION_VALUE:-$(t skipped_word)}"
    _nap 0.05
    _panel_row "$(t s_config)"   "${CONFIG_VALUE:-$(t skipped_word)}"
    _nap 0.05
    _panel_row "$(t s_mode)"     "${MODE_VALUE:-}"
    _nap 0.05
    _panel_row "$(t s_time)"     "$(_fmt_ms "$TOTAL_MS")"
    printf '  %s╰%s╯%s\n' "$GREY" "$h" "$RESET"
    return 0
}

# ── rust (distro-aware) ──────────────────────────────────────────────────────
_suggest_rust_install() {
    case "$DISTRO_ID" in
        arch|endeavouros|manjaro) echo "sudo pacman -S rustup && rustup default stable" ;;
        ubuntu|debian|linuxmint|pop|elementary|zorin) echo "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y  # o: sudo apt install -y cargo" ;;
        fedora|rhel|centos|almalinux|rocky) echo "sudo dnf install -y cargo  # o: rustup" ;;
        opensuse*|suse) echo "sudo zypper install -y cargo" ;;
        void) echo "sudo xbps-install -S rust" ;;
        alpine) echo "sudo apk add cargo rust" ;;
        nixos) echo "nix-shell -p rustup  # o: rustup via home-manager" ;;
        gentoo) echo "sudo emerge dev-lang/rust" ;;
        *) echo "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y" ;;
    esac
    return 0
}

_spin_wait() {
    local pid=$1 label="$2" i=0
    if [[ $HAS_TTY -ne 1 ]]; then
        while kill -0 "$pid" 2>/dev/null; do sleep 1; done
        return 0
    fi
    while kill -0 "$pid" 2>/dev/null; do
        local f="${SPIN_ORBIT[$(( i % ${#SPIN_ORBIT[@]} ))]}"
        printf '\r  %s%s%s  %s%s%s\e[K' "$ACCENT" "$f" "$RESET" "$DIM" "$label" "$RESET"
        if [[ $ANIM -eq 1 ]]; then sleep 0.12; else sleep 0.5; fi
        i=$(( i + 1 ))
    done
    printf '\r\e[K'
    return 0
}

ensure_rust() {
    if command -v cargo >/dev/null 2>&1; then
        local ver_raw maj min
        ver_raw="$(rustc --version 2>/dev/null || printf 'rustc 0.0')"
        maj="$(printf '%s' "$ver_raw" | grep -oE '[0-9]+\.[0-9]+' | head -n1 | cut -d. -f1 || true)"
        min="$(printf '%s' "$ver_raw" | grep -oE '[0-9]+\.[0-9]+' | head -n1 | cut -d. -f2 || true)"
        if [[ -n "${maj:-}" && -n "${min:-}" && "$maj" =~ ^[0-9]+$ && "$min" =~ ^[0-9]+$ ]]; then
            if (( maj < 1 )) || { (( maj == 1 )) && (( min < 82 )); }; then
                printf '  %s⡿⠿%s  %s — rustc %s.%s\n' "$YELLOW" "$RESET" "$(t need_rust)" "$maj" "$min"
            fi
        fi
        return 0
    fi
    printf '\n  %s⡿⠿%s  %s\n' "$YELLOW" "$RESET" "$(t need_rust)"
    printf '  %s  ⠤ %s%s\n' "$DIM" "$(_suggest_rust_install)" "$RESET"
    if [[ "$YES" == true || $HAS_TTY -ne 1 ]]; then
        return 1
    fi
    ask "$(t install_rust_q)" "$(t install_rust_hint)"
    if ! is_yes no; then return 1; fi
    printf '  %s⠤ %s…%s\n' "$CYAN" "$(t installing_rust)" "$RESET"
    if ! command -v curl >/dev/null 2>&1; then
        printf '  %s⡿⠿%s  curl not found — %s\n' "$YELLOW" "$RESET" "$(t rust_fail)"
        return 1
    fi
    local log
    log="$(mktemp /tmp/maverick-rustup.XXXXXX)"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --no-modify-path >"$log" 2>&1 &
    local rpid=$!
    _spin_wait "$rpid" "$(t installing_rust)…"
    local rc=0
    wait "$rpid" || rc=$?
    if [[ $rc -eq 0 && -f "$HOME/.cargo/env" ]]; then
        # shellcheck disable=SC1090
        . "$HOME/.cargo/env" 2>/dev/null || true
    fi
    export PATH="$HOME/.cargo/bin:$PATH"
    if command -v cargo >/dev/null 2>&1; then
        printf '  %s⣦⠞%s  %s\n' "$GREEN" "$RESET" "$(t rust_ok)"
        rm -f "$log"
        return 0
    fi
    printf '  %s⡿⠿%s  %s\n' "$YELLOW" "$RESET" "$(t rust_fail)"
    tail -n 8 "$log" 2>/dev/null | sed 's/^/  /' || true
    return 1
}

# ── NEW: Disk space check ────────────────────────────────────────────────────
check_disk_space() {
    local target_dir="$1"
    local min_kb=2097152  # 2GB in KB
    
    # Obtener espacio disponible en KB usando df POSIX
    local avail_kb
    avail_kb=$(df -Pk "$target_dir" 2>/dev/null | awk 'NR==2 {print $4}')
    
    if [[ -z "$avail_kb" || ! "$avail_kb" =~ ^[0-9]+$ ]]; then
        # No se pudo determinar, continuar con advertencia
        printf '  %s⚠%s  could not check disk space\n' "$YELLOW" "$RESET"
        return 0
    fi
    
    if (( avail_kb < min_kb )); then
        local avail_mb=$(( avail_kb / 1024 ))
        printf '  %s⡱⢎%s  %s (%d MB)\n' "$RED" "$RESET" "$(t disk_space_warn)" "$avail_mb"
        if [[ "$YES" != true && -t 0 ]]; then
            ask "Continue anyway?" "[y/N]"
            if ! is_yes no; then
                printf '  %s%s%s\n' "$DIM" "$(t aborted)" "$RESET"
                exit 0
            fi
        fi
        return 1
    fi
    return 0
}

# ─────────────────────────────────────────────────────────────────────────────
#  GO
# ─────────────────────────────────────────────────────────────────────────────
check_unsupported_os

# Column alignment degrades to bytes when wc is unavailable; say so once.
if [[ $HAS_WC -eq 0 ]]; then
    printf '  %s⚠%s  %s\n' "$YELLOW" "$RESET" "$(t wc_missing)"
fi

_now_ms
T_START=$NOW_MS
banner
confirm

# ── config pre-check ─────────────────────────────────────────────────────────
CFG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/maverick"
CFG_FILE="$CFG_DIR/config.toml"
CONFIG_ACTION="create"
if [[ "$NO_CONFIG" == true ]]; then
    CONFIG_ACTION="skip"
elif [[ -f "$CFG_FILE" ]]; then
    if [[ "$YES" == true ]]; then
        CONFIG_ACTION="keep"
    else
        printf '  %s⠆ %s: %s%s\n' "$YELLOW" "$(t config_found)" "$CFG_FILE" "$RESET"
        ask "$(t config_overwrite_q)" "$(t config_overwrite_hint)"
        if is_yes no; then
            CONFIG_ACTION="overwrite"
            printf '  %s⠤ %s%s\n' "$DIM" "$(t config_will_overwrite)" "$RESET"
        else
            CONFIG_ACTION="keep"
            printf '  %s⠤ %s%s\n' "$DIM" "$(t config_keep)" "$RESET"
        fi
        echo
    fi
fi

# ── compositor (una sola línea) ──────────────────────────────────────────────
if [[ -z "$WITH_COMPOSITOR" ]]; then
    if [[ "$YES" == true || "$NO_BUILD" == true ]]; then
        WITH_COMPOSITOR="no"
    else
        ask "$(t compositor_q)" "$(t compositor_hint)"
        if is_yes no; then
            WITH_COMPOSITOR="yes"
            printf '  %s⠤ %s%s\n' "$DIM" "$(t compositor_yes)" "$RESET"
        else
            WITH_COMPOSITOR="no"
            printf '  %s⠤ %s%s\n' "$DIM" "$(t compositor_no)" "$RESET"
        fi
        echo
    fi
fi
if [[ "$WITH_COMPOSITOR" == "no" ]]; then
    CARGO_FEATURES="--no-default-features"
    MODE_VALUE="$(t comp_no_short)"
else
    CARGO_FEATURES=""
    MODE_VALUE="$(t comp_yes_short)"
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
    check_disk_space "$APP_DIR" || true
fi

# ── abrir el bloque vivo ─────────────────────────────────────────────────────
_phases_init
echo
if [[ $HAS_TTY -eq 1 ]]; then
    printf '\e[?25l'
    CURSOR_HIDDEN=1
fi
_render ""
_nap 0.25

# ── fase 0 · dependencias ────────────────────────────────────────────────────
_phase_begin 0 "$DEPS_DETAIL"
if [[ "$NO_BUILD" != true ]]; then
    command -v cargo >/dev/null 2>&1 || die "$(t need_cargo)"
fi
# System link check (only when we are about to compile). The GLX FFI links
# libX11 and libX11-xcb; without them `cargo build` dies late with a cryptic
# linker error, so fail here with the distro package names instead.
#
# Only what the installed binaries actually link is probed. libXcomposite is
# not among them — it is used solely by the C test client in tests/, so
# requiring its -dev package would block a normal installation over a
# development-only dependency.
if [[ "$NO_BUILD" != true ]]; then
    if ! command -v cc >/dev/null 2>&1; then
        die "no C linker (cc) — Arch: pacman -S base-devel · Debian: apt install build-essential · Fedora: dnf groupinstall 'Development Tools'"
    fi
    _x11_probe="$(mktemp /tmp/maverick-x11probe.XXXXXX.c)"
    printf 'int XOpenDisplay(); int main(void){return XOpenDisplay();}\n' >"$_x11_probe"
    if ! cc -o "${_x11_probe}.out" "$_x11_probe" -lX11 -lX11-xcb >/dev/null 2>&1; then
        rm -f "$_x11_probe" "${_x11_probe}.out"
        die "X11 client libraries not linkable (-lX11 -lX11-xcb) — Arch: pacman -S libx11 · Debian: apt install libx11-dev libxcb1-dev · Fedora: dnf install libX11-devel libxcb-devel"
    fi
    rm -f "$_x11_probe" "${_x11_probe}.out"
fi
_animate 8 "$DEPS_DETAIL"
_nap 0.2
_phase_end 0 "$DEPS_DETAIL"

# ── fase 1 · build ───────────────────────────────────────────────────────────
FINAL_LOG_PATH=""
if [[ "$NO_BUILD" == true ]]; then
    _animate 64 ""
    _phase_skip 1
else
    _phase_begin 1 "$(t building_detail)"
    BUILD_LOG="$(mktemp /tmp/maverick-build.XXXXXX)"

    # Progress needs a denominator. Count the real dependency tree for the
    # packages and features actually being compiled; a fixed estimate is only
    # the last resort when `cargo tree` cannot answer, because an estimate that
    # can only grow leaves the bar pinned below 100% for the whole build.
    # shellcheck disable=SC2086
    estimated_crates="$(cargo tree --edges normal,build --prefix none $CARGO_FEATURES \
        -p maverick -p maverick-sys 2>/dev/null | sort -u | grep -c . || true)"
    if [[ -z "$estimated_crates" || "$estimated_crates" -lt 1 ]]; then
        if [[ "$WITH_COMPOSITOR" == "no" ]]; then
            estimated_crates=12
        else
            estimated_crates=75
        fi
    fi
    total_crates=$estimated_crates
    
    # shellcheck disable=SC2086
    (
        if RUSTFLAGS="-C target-cpu=native" cargo build --release $CARGO_FEATURES \
               -p maverick -p maverick-sys >"$BUILD_LOG" 2>&1; then
            exit 0
        fi
        # shellcheck disable=SC2086
        cargo build --release $CARGO_FEATURES \
            -p maverick -p maverick-sys >>"$BUILD_LOG" 2>&1
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
                printf '  … %s %s/%s\n' "$(t phase_build)" "$compiled" "$total_crates"
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
    _phase_end 1 "$(t build_ok)"
fi

# ── fase 2 · binarios ────────────────────────────────────────────────────────
_phase_begin 2 ""
# Check the complete artifact set before replacing any installed binary.
for bin in "${RUNTIME_BINS[@]}"; do
    [[ -f "$CARGO_TARGET_DIR/release/$bin" && -x "$CARGO_TARGET_DIR/release/$bin" ]] ||
        die "missing executable $CARGO_TARGET_DIR/release/$bin (run without --no-build)"
done
"$CARGO_TARGET_DIR/release/maverick" --version >/dev/null 2>&1 || die "$(t verify_fail)"
install_command mkdir -p -- "$BIN_DIR" || die "$(t no_write): $BIN_DIR"

# Stage the whole set before replacing anything. Committing one binary at a
# time could leave a prefix holding a new maverick beside an old maverickctl,
# which is a version-skewed pair that still type-checks and still misbehaves.
# Staging first means a missing artifact, a full disk or a bad destination
# fails while the installed prefix is still entirely the previous one.
STAGE_DIR="$BIN_DIR/.maverick-stage.$$"
install_command mkdir -p -- "$STAGE_DIR" || die "$(t no_write): $STAGE_DIR"
for bin in "${RUNTIME_BINS[@]}"; do
    install_command install -m 0755 -- "$CARGO_TARGET_DIR/release/$bin" "$STAGE_DIR/$bin" \
        || { stage_cleanup; die "stage failed: $bin"; }
done
# Verify the staged set before any of it becomes the installed set. A
# destination that is a directory rather than a file is caught here, while the
# rename that would have failed is still ahead of us.
for bin in "${RUNTIME_BINS[@]}"; do
    [[ -f "$STAGE_DIR/$bin" && -x "$STAGE_DIR/$bin" ]] \
        || { stage_cleanup; die "staged binary is not executable: $bin"; }
    if [[ -d "$BIN_DIR/$bin" && ! -L "$BIN_DIR/$bin" ]]; then
        stage_cleanup
        die "$BIN_DIR/$bin is a directory; refusing to replace it"
    fi
done

# Commit. Each move is a rename within one directory, so a running executable
# is never truncated (ETXTBSY) and the set lands as a unit.
n_ok=0
for bin in "${RUNTIME_BINS[@]}"; do
    install_command mv -fT -- "$STAGE_DIR/$bin" "$BIN_DIR/$bin" \
        || { stage_cleanup; die "install failed: $bin"; }
    n_ok=$(( n_ok + 1 ))
    if (( OVERALL < 78 )); then OVERALL=$(( OVERALL + 2 )); fi
    _render "$bin"
    _nap 0.05
done
rm -rf -- "$STAGE_DIR"
STAGE_DIR=""
_animate 80 "$n_ok/$RUNTIME_BIN_COUNT"
_phase_end 2 "$n_ok/$RUNTIME_BIN_COUNT · $(t installed)"

# ── fase 3 · sesión X11 ──────────────────────────────────────────────────────
_phase_begin 3 ""
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
if [[ "$WITH_COMPOSITOR" == "no" ]]; then
    session_comment='Columnar tiling WM — keyboard-driven'
else
    session_comment='Columnar tiling WM — scrollable, composited, keyboard-driven'
fi
printf '%s\n' \
    '[Desktop Entry]' \
    'Name=maverick' \
    "Comment=$session_comment" \
    "Exec=\"$exec_path\"" \
    'Type=Application' > "$INSTALL_TMP/maverick.desktop"
install_command mkdir -p -- "$XS_DIR" || die "$(t no_write): $XS_DIR"
install_command install -m 0644 -- "$INSTALL_TMP/maverick.desktop" "$XS_DIR/maverick.desktop" \
    || die "session install failed: $XS_DIR/maverick.desktop"
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
    install_command mkdir -p -- "$xsessions_abs" || die "$(t no_write): $xsessions_abs"
    install_command install -m 0644 -- "$INSTALL_TMP/maverick.desktop" \
        "$xsessions_abs/maverick.desktop" \
        || die "session install failed: $xsessions_abs/maverick.desktop"
    SESSION_VALUE="$xsessions_abs/maverick.desktop"
fi
_animate 84 "$SESSION_VALUE"
_phase_end 3 "$SESSION_VALUE"

# ── fase 4 · configuración ───────────────────────────────────────────────────
_phase_begin 4 ""
CONFIG_VALUE=""
case "$CONFIG_ACTION" in
    skip)
        _animate 88 "$(t config_skip)"
        _phase_skip 4 "$(t config_skip)"
        ;;
    keep)
        _animate 88 "$(t config_keep)"
        CONFIG_VALUE="$CFG_FILE"
        _phase_end 4 "$(t config_keep)"
        ;;
    create|overwrite)
        _animate 88 "$CFG_FILE"
        if ! mkdir -p "$CFG_DIR" 2>/dev/null; then
            die "$(t no_write): $CFG_DIR"
        fi
        wrote=0
        if [[ -f "$APP_DIR/config/config.toml" ]]; then
            if cp "$APP_DIR/config/config.toml" "$CFG_FILE" 2>/dev/null; then
                wrote=1
            fi
        fi
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
        _phase_end 4 "${CFG_FILE}${cfg_ok}"
        ;;
esac

# ── fase 5 · verificación final ──────────────────────────────────────────────
_phase_begin 5 ""

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
verify_installed() {
    local label="$1"; shift
    if ! "$@" >/dev/null 2>&1; then
        die "$(t verify_fail): $label"
    fi
    VERIFY_CHECKS=$(( VERIFY_CHECKS + 1 ))
}
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
_phase_end 5 "$VERIFY_CHECKS ✓ · $verify_detail"
_nap 0.35

# ── cerrar el bloque ─────────────────────────────────────────────────────────
printf '\n'
if [[ $CURSOR_HIDDEN -eq 1 ]]; then
    printf '\e[?25h'
    CURSOR_HIDDEN=0
fi
BLOCK_OPEN=0
_now_ms
TOTAL_MS=$(( NOW_MS - T_START ))

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
if [[ ":$PATH:" != *":$BIN_DIR:"* ]]; then
    echo
    printf '  %s⡿⠿%s  %s: %s\n' "$YELLOW" "$RESET" "$BIN_DIR" "$(t not_in_path)"
    printf '  %s  export PATH="%s:$PATH"%s\n' "$DIM" "$BIN_DIR" "$RESET"
fi

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
