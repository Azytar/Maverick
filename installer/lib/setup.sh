#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  MAVERICK installer — environment (lib/setup.sh)
#
#  What the machine already has, and what has to be arranged before the build
#  can start: platform and distro, the Rust toolchain, free disk space, the
#  X11 link probe, and putting the installed binaries on PATH.
#
#  Contract: sourcing defines, os_detect() probes. Nothing here ever
#  escalates — a missing tool or an unwritable path is reported, not worked
#  around with sudo.
# ─────────────────────────────────────────────────────────────────────────────

# ── platform ─────────────────────────────────────────────────────────────────
os_detect() {
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
}

# ── unsupported systems ──────────────────────────────────────────────────────
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

# ── disk space ───────────────────────────────────────────────────────────────
# Advisory: a full disk otherwise fails deep inside the build, so the check
# runs first and reports. It never blocks — there is no recovery to offer
# here, and a warning the caller can act on beats a refusal.
check_disk_space() {
    local target_dir="$1"
    local min_kb=2097152  # 2GB in KB
    
    # Obtener espacio disponible en KB usando df POSIX
    local avail_kb
    avail_kb=$(df -Pk "$target_dir" 2>/dev/null | awk 'NR==2 {print $4}' || true)
    
    if [[ -z "$avail_kb" || ! "$avail_kb" =~ ^[0-9]+$ ]]; then
        # No se pudo determinar, continuar con advertencia
        printf '  %s⚠%s  %s\n' "$YELLOW" "$RESET" "$(t disk_unknown)"
        return 0
    fi
    
    if (( avail_kb < min_kb )); then
        local avail_mb=$(( avail_kb / 1024 ))
        printf '  %s⡱⢎%s  %s (%d MB)\n' "$RED" "$RESET" "$(t disk_space_warn)" "$avail_mb"
        # Not a confirmation like the others: keeping the guard is what
        # makes --yes mean "go on" here, where the default is "stop".
        if [[ "$YES" != true && -t 0 ]]; then
            ask "$(t disk_continue_q)" "$(t disk_continue_hint)"
            if ! is_yes no; then
                printf '  %s%s%s\n' "$DIM" "$(t aborted)" "$RESET"
                exit 0
            fi
        fi
        return 0
    fi
    return 0
}

# ── X11 link probe ───────────────────────────────────────────────────────────
# System link check, run only when we are about to compile. The GLX FFI links
# libX11 and libX11-xcb; without them `cargo build` dies late with a cryptic
# linker error, so this fails here instead, naming the distro packages.
#
# Only what the installed binaries actually link is probed. libXcomposite is
# not among them — it is used solely by the C test client in tests/, so
# requiring its -dev package would block a normal installation over a
# development-only dependency.
check_x11_linkable() {
    if ! command -v cc >/dev/null 2>&1; then
        die "$(t err_cc)"
    fi
    local probe
    probe="$(mktemp /tmp/maverick-x11probe.XXXXXX.c)"
    printf 'int XOpenDisplay(); int main(void){return XOpenDisplay();}\n' >"$probe"
    if ! cc -o "${probe}.out" "$probe" -lX11 -lX11-xcb >/dev/null 2>&1; then
        rm -f "$probe" "${probe}.out"
        die "$(t err_x11)"
    fi
    rm -f "$probe" "${probe}.out"
    return 0
}

# ── PATH ─────────────────────────────────────────────────────────────────────
# The default prefix installs into $HOME/.local/bin, which plenty of
# distributions do not put on PATH. A person who has just run an installer
# should not have to edit a startup file by hand to run what it installed, so
# the installer does that part too: one marked block, in the two files the
# caller's shell actually reads, guarded so it is a no-op once the directory
# is there.
#
# Three properties keep this from being the kind of edit that teaches people
# to distrust installers. It is asked for first (and refused outright with
# --no-path). It never leaves $HOME. And it never answers a question that has
# been answered: a startup file that already names the directory is reported
# rather than rewritten, so a distribution's own ~/.local/bin line is respected
# instead of doubled up.
PATH_MARKER_BEGIN='# >>> maverick (install.sh) >>>'
PATH_MARKER_END='# <<< maverick (install.sh) <<<'
PATH_FILES=()
PATH_RESULT=""   # in-path | configured | added | refused | failed
PATH_VALUE=""
PATH_FAILED=""
# Every startup file the installer understands, for the "is this already
# handled?" probe. A file outside this set is never read, never written.
PATH_KNOWN_FILES=(
    "${HOME:-}/.profile" "${HOME:-}/.bash_profile" "${HOME:-}/.bash_login" "${HOME:-}/.bashrc"
    "${HOME:-}/.zshenv" "${HOME:-}/.zprofile" "${HOME:-}/.zshrc"
    "${XDG_CONFIG_HOME:-${HOME:-}/.config}/fish/config.fish"
)

# The block itself — POSIX or fish, depending on the file it is going into.
# The directory is expanded and quoted so the guard matches in a shell where
# $HOME is not exported and so a prefix containing spaces (which the installer
# otherwise supports end to end) stays one word here too.
path_block() {
    local file="$1" dir="$BIN_DIR"
    dir="${dir//\\/\\\\}"
    dir="${dir//\"/\\\"}"
    printf '%s\n' "$PATH_MARKER_BEGIN"
    printf '%s\n' '# Added by install.sh — delete these lines to undo.'
    if [[ "$file" == *"/fish/"* ]]; then
        printf 'if not contains -- "%s" $PATH\n' "$dir"
        printf '    set -gx PATH "%s" $PATH\n' "$dir"
        printf 'end\n'
    else
        printf 'case ":$PATH:" in\n'
        printf '  *":%s:"*) ;;\n' "$dir"
        printf '  *) PATH="%s:$PATH"; export PATH ;;\n' "$dir"
        printf 'esac\n'
    fi
    printf '%s\n' "$PATH_MARKER_END"
    return 0
}

# The directory is under $HOME, so a startup file may name it three ways: as
# it is, or with $HOME spelled with braces or without. Recognising only the
# expanded form would call a distribution's own line "unconfigured" and append
# a second answer to a working one.
path_dir_is_named() {
    local f rest
    local -a forms=("$BIN_DIR") args=()
    if [[ -n "${HOME:-}" && "$BIN_DIR" == "$HOME/"* ]]; then
        rest="${BIN_DIR#"$HOME"/}"
        forms+=('${HOME}/'"$rest" '$HOME/'"$rest")
    fi
    # An empty pattern would match every line, so the arguments are built
    # first and only from forms that actually exist.
    for f in "${forms[@]}"; do args+=(-e "$f"); done
    for f in "${PATH_KNOWN_FILES[@]}"; do
        [[ -f "$f" ]] || continue
        if grep -qF "${args[@]}" -- "$f" 2>/dev/null; then
            return 0
        fi
    done
    return 1
}

# Which files this caller's shell reads: its login file and its interactive
# rc. The choice follows $SHELL rather than the parent process, because what
# has to work is the next terminal the user opens, not this one.
path_target_files() {
    local name login="" rc=""
    name="${SHELL:-}"
    name="${name##*/}"
    [[ -n "$name" ]] || name="sh"
    case "$name" in
        bash)
            # bash reads the first of these three for a login shell, and
            # ~/.bashrc for an interactive non-login one.
            if   [[ -f "$HOME/.bash_profile" ]]; then login="$HOME/.bash_profile"
            elif [[ -f "$HOME/.bash_login" ]];   then login="$HOME/.bash_login"
            else                                       login="$HOME/.profile"; fi
            rc="$HOME/.bashrc" ;;
        zsh)
            # zsh reads no POSIX profile of its own, so ~/.profile is chosen
            # only when it is already there; ~/.zshrc is created if missing,
            # because it is the file an interactive zsh reads either way.
            if   [[ -f "$HOME/.zprofile" ]]; then login="$HOME/.zprofile"
            elif [[ -f "$HOME/.profile" ]];  then login="$HOME/.profile"; fi
            rc="$HOME/.zshrc" ;;
        fish)
            rc="${XDG_CONFIG_HOME:-$HOME/.config}/fish/config.fish" ;;
        csh|tcsh|nu|elvish|xonsh)
            # These read no POSIX startup file, and writing POSIX syntax into
            # the one they do read would break it instead of adding a PATH
            # entry. Nothing to target: the export line is the honest answer.
            PATH_FILES=()
            return 0 ;;
        *)
            login="$HOME/.profile" ;;
    esac
    PATH_FILES=()
    local f
    for f in "$login" "$rc"; do
        # A startup file outside $HOME is not this installer's to edit (an
        # XDG_CONFIG_HOME pointed at a system path, say). No writes there:
        # the caller gets the export line instead.
        [[ -n "$f" && "$f" == "$HOME/"* ]] || continue
        if [[ ${#PATH_FILES[@]} -gt 0 && "${PATH_FILES[${#PATH_FILES[@]}-1]}" == "$f" ]]; then
            continue
        fi
        PATH_FILES+=("$f")
    done
    return 0
}

# Rewrite one startup file with the current block. A block a previous install
# left behind is dropped first, so the file holds exactly one answer, and the
# file is rewritten in place rather than replaced: mode, owner and inode are
# not this installer's to change.
path_write_block() {
    local file="$1" tmp
    mkdir -p -- "$(dirname -- "$file")" 2>/dev/null || return 1
    tmp="$(mktemp "${INSTALL_TMP:-${TMPDIR:-/tmp}}/path.XXXXXX" 2>/dev/null)" || return 1
    if [[ -f "$file" ]]; then
        awk -v b="$PATH_MARKER_BEGIN" -v e="$PATH_MARKER_END" '
            $0 == b { skip = 1; next }
            $0 == e { skip = 0; next }
            skip    { next }
                      { print }
        ' "$file" >"$tmp" || { rm -f -- "$tmp"; return 1; }
    fi
    path_block "$file" >>"$tmp" || { rm -f -- "$tmp"; return 1; }
    cat "$tmp" >"$file" || { rm -f -- "$tmp"; return 1; }
    rm -f -- "$tmp"
    return 0
}

# The files as the summary shows them: shortened against $HOME so the line
# fits a panel and reads like a path the user recognises.
path_files_display() {
    local out="" f s
    for f in "${PATH_FILES[@]}"; do
        if [[ -n "${HOME:-}" && "$f" == "$HOME/"* ]]; then
            s="~${f#"$HOME"}"
        else
            s="$f"
        fi
        [[ -z "$out" ]] || out+=" · "
        out+="$s"
    done
    printf '%s' "$out"
    return 0
}

# Decided, prompted and (when accepted) written. The result is reported by the
# summary rather than here, so the decision is taken after the install is
# known to have succeeded and before the panel that describes it.
path_setup() {
    PATH_FILES=()
    PATH_VALUE="$(t skipped_word)"
    PATH_RESULT="refused"
    PATH_FAILED=""

    # The common case must cost nothing: a distribution that already ships
    # this directory on PATH, or a --system install into /usr/local, gets no
    # prompt and no file.
    if [[ ":${PATH:-}" == *":$BIN_DIR:"* ]]; then
        PATH_RESULT="in-path"
        PATH_VALUE="$(t path_ok)"
        return 0
    fi

    [[ "$PATH_CHOICE" != "no" ]] || return 0

    path_target_files
    if [[ ${#PATH_FILES[@]} -eq 0 ]]; then
        # No file this installer may write — a shell with no POSIX startup
        # file, or one configured to keep its rc outside $HOME. Nothing was
        # attempted, so this reads as a refusal rather than a failure.
        PATH_RESULT="refused"
        return 0
    fi

    # The directory is named by a startup file already: the answer exists and
    # this shell is simply older than it. Appending would duplicate a line
    # that works, so say where it is and leave every file untouched.
    if path_dir_is_named; then
        PATH_RESULT="configured"
        PATH_VALUE="$(path_files_display)"
        return 0
    fi

    # No terminal, no answer to give: take the default, exactly as the install
    # confirmation itself does. --add-path and --yes skip the question too.
    if [[ "$PATH_CHOICE" != "yes" && "$YES" != true && -t 0 ]]; then
        ask "$(t path_q) $(path_files_display)?" "$(t confirm_hint)"
        if ! is_yes yes; then
            echo
            return 0
        fi
        echo
    fi

    local f ok=0
    for f in "${PATH_FILES[@]}"; do
        if path_write_block "$f"; then
            ok=$(( ok + 1 ))
        elif [[ -z "$PATH_FAILED" ]]; then
            PATH_FAILED="$f"
        fi
    done
    if (( ok == 0 )); then
        PATH_RESULT="failed"
        PATH_VALUE="$(t skipped_word)"
        return 0
    fi
    PATH_RESULT="added"
    PATH_VALUE="$(path_files_display)"
    if [[ -n "$PATH_FAILED" ]]; then
        printf '  %s⡿⠿%s  %s %s\n' "$YELLOW" "$RESET" "$PATH_FAILED" "$(t path_write_fail)"
    fi
    return 0
}
