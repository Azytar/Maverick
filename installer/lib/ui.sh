#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  MAVERICK installer — presentation (lib/ui.sh)
#
#  Everything that draws or asks lives here: terminal probing, colours, the
#  banner, the live step block, the spinners, the celebration effects, the
#  summary panel, and the one prompt helper every yes/no question goes
#  through.
#
#  Contract: sourcing defines, ui_init() probes. Nothing in this file writes
#  a file, picks what to install or escalates — it prints, and returns a
#  status the caller decides what to do with.
# ─────────────────────────────────────────────────────────────────────────────

# ── terminal probe ───────────────────────────────────────────────────────────
# Colour, width and animation are decided once, here. Everything below reads
# these variables and degrades on its own: no tty means no colour, no cursor
# games, and no question asked of nobody.
ui_init() {
    if [[ -t 1 && -z "${NO_COLOR:-}" && "${TERM:-}" != "dumb" ]]; then
        HAS_TTY=1
    else
        HAS_TTY=0
    fi
    ANIM=$HAS_TTY
    if [[ "${NO_ANIM:-0}" == "1" || "${NO_ANIM:-0}" == "true" || "${NO_ANIM:-0}" == "yes" ]]; then
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
    return 0
}

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

# ui_init leaves the colours empty when there is no terminal, so every printf
# below already carries an empty escape and no caller has to ask whether
# colour survived.

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

# The live block hides the cursor; this puts it back. install.sh calls it
# from its exit handler so that a failure or a Ctrl-C never leaves a shell
# without one.
ui_cursor_restore() {
    if [[ ${CURSOR_HIDDEN:-0} -eq 1 ]]; then
        printf '\e[?25h' >&2 || true
        CURSOR_HIDDEN=0
    fi
    return 0
}

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


# ── prompt: the confirmation policy, once ────────────────────────────────────
# --yes and a missing terminal are the same answer: the default, resolved
# silently, because neither can answer a question. That used to be four
# separate re-statements of the same rule; now a caller asks for a default
# and gets 0 for yes, 1 for no. The closing newline is printed only when the
# question was really asked, so an unattended run never grows blank lines.
prompt() {
    local question="$1" hint="$2" default="${3:-yes}" rc=1
    if [[ "$YES" == true || ! -t 0 ]]; then
        REPLY=""
        is_yes "$default" && rc=0
        return "$rc"
    fi
    ask "$question" "$hint"
    echo
    rc=1
    is_yes "$default" && rc=0
    return "$rc"
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
N_STEPS=6
ST_LABEL=(); ST_STATE=(); ST_DETAIL=(); ST_TIME=(); ST_T0=()
OVERALL=0
ST_FLASH=-1
LIVE_ETA=""

_steps_init() {
    ST_LABEL=( "$(t step_deps)" "$(t step_build)" "$(t step_install)" \
               "$(t step_session)" "$(t step_config)" "$(t step_final)" )
    ST_STATE=(0 0 0 0 0 0)
    ST_DETAIL=("" "" "" "" "" "")
    ST_TIME=("" "" "" "" "" "")
    ST_T0=(0 0 0 0 0 0)
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
        printf '\e[%dA\r' "$(( N_STEPS + 1 ))"
    fi
    BLOCK_OPEN=1
    for ((i=0; i<N_STEPS; i++)); do
        local mark="·" mcol="$GREY" lcol="$DIM" dcol="$DIM" tcol="$GREY"
        local det="${ST_DETAIL[$i]}" lbl="${ST_LABEL[$i]}" tstr=""
        case "${ST_STATE[$i]}" in
            1)
                _spin_pick "$i"
                mark="$SPIN_CHAR"; mcol="$ACCENT"; dcol="$RESET"
                if [[ -n "$live" ]]; then det="$live"; fi
                if (( ${ST_T0[$i]} > 0 )); then
                    local el=$(( now - ${ST_T0[$i]} ))
                    if (( el < 0 )); then el=0; fi
                    tstr="$(_fmt_ms "$el")"
                fi
                ;;
            2) mark="✓"; mcol="$GREEN"; lcol="$RESET"; tstr="${ST_TIME[$i]}" ;;
            3) mark="○"; mcol="$GREY" ;;
        esac
        if [[ $i -eq $ST_FLASH ]]; then mark="⠿"; mcol="$GREEN"; fi
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

_step_begin() {
    ST_STATE[$1]=1
    ST_DETAIL[$1]="${2:-}"
    _now_ms
    ST_T0[$1]=$NOW_MS
    _render "${2:-}"
    return 0
}

_step_end() {
    local i=$1
    ST_STATE[$i]=2
    ST_DETAIL[$i]="${2:-}"
    _now_ms
    ST_TIME[$i]="$(_fmt_ms $(( NOW_MS - ${ST_T0[$i]} )))"
    LIVE_ETA=""
    if [[ $HAS_TTY -ne 1 ]]; then
        printf '  ✓ %s — %s (%s)\n' "${ST_LABEL[$i]}" "${ST_DETAIL[$i]}" "${ST_TIME[$i]}"
        return 0
    fi
    if [[ $ANIM -eq 1 ]]; then
        ST_FLASH=$i
        _render ""
        sleep 0.05
        ST_FLASH=-1
    fi
    _render ""
    return 0
}

_step_skip() {
    ST_STATE[$1]=3
    ST_DETAIL[$1]="${2:-$(t skipped_word)}"
    if [[ $HAS_TTY -ne 1 ]]; then
        printf '  ○ %s — %s\n' "${ST_LABEL[$1]}" "${ST_DETAIL[$1]}"
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
    _panel_row "$(t s_path)"     "${PATH_VALUE:-$(t skipped_word)}"
    _nap 0.05
    _panel_row "$(t s_session)"  "${SESSION_VALUE:-$(t skipped_word)}"
    _nap 0.05
    _panel_row "$(t s_config)"   "${CONFIG_VALUE:-$(t skipped_word)}"
    _nap 0.05
    _panel_row "$(t s_time)"     "$(_fmt_ms "$TOTAL_MS")"
    printf '  %s╰%s╯%s\n' "$GREY" "$h" "$RESET"
    return 0
}
