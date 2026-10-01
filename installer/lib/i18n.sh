#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  MAVERICK installer — language (lib/i18n.sh)
#
#  One table per language, one lookup, one audit. Adding a message is adding
#  two lines; running this file directly (bash lib/i18n.sh) fails when the
#  two tables drift apart or when the installer asks for a key that no table
#  defines, which is otherwise a typo that prints itself on screen.
#
#  Sourcing this file only declares things. i18n_init reads the environment
#  and is called by install.sh once --lang has been parsed.
# ─────────────────────────────────────────────────────────────────────────────

# ── language ─────────────────────────────────────────────────────────────────
detect_lang() {
        local src="${LC_ALL:-}${LC_MESSAGES:-}${LANG:-} $1"
        src="$(printf '%s' "$src" | command tr '[:upper:]' '[:lower:]')"
        if [[ "$src" == *"es"* ]]; then printf 'es'; else printf 'en'; fi
        return 0
}

LANG_ID="en"

# Called once, after the command line has been parsed: the answer depends on
# --lang, which nothing above this line has seen yet.
i18n_init() {
    if [[ "${LANG_CHOICE:-auto}" == "auto" ]]; then
        LANG_ID="$(detect_lang "")"
    else
        case "$LANG_CHOICE" in
            es|es_*|spanish) LANG_ID="es" ;;
            *)               LANG_ID="en" ;;
        esac
    fi
    return 0
}

# ── message tables ───────────────────────────────────────────────────────────
declare -gA MSG_EN=()
MSG_EN[banner_sub]='columnar tiling window manager'
MSG_EN[detect_lang]='language detected'
MSG_EN[os_detected]='system detected'
MSG_EN[distro_detected]=distro
MSG_EN[dest]=prefix
MSG_EN[confirm_q]='Install Maverick to'
MSG_EN[confirm_hint]='[Y/n]'
MSG_EN[aborted]=aborted.
MSG_EN[need_cargo]='cargo not found in PATH — install rustup from https://rustup.rs'
MSG_EN[need_rust]='Rust not found — cargo ≥ 1.82 required'
MSG_EN[install_rust_q]='Install Rust now?'
MSG_EN[install_rust_hint]='[y/N]'
MSG_EN[installing_rust]='installing Rust…'
MSG_EN[rust_ok]='Rust installed'
MSG_EN[rust_fail]='automatic Rust install failed'
MSG_EN[unsupported]='unsupported system'
MSG_EN[use_linux]='Maverick is an X11 window manager for Linux only. On Windows, use WSL2 with a Linux distro.'
MSG_EN[x11_only]='requires Linux with X11 (X.Org / XLibre)'
MSG_EN[phase_deps]='Checking dependencies'
MSG_EN[phase_build]=Building
MSG_EN[phase_install]='Installing binaries'
MSG_EN[phase_session]='X11 session'
MSG_EN[phase_config]=Configuration
MSG_EN[phase_final]='Final verification'
MSG_EN[deps_ok]='all good'
MSG_EN[building_detail]='this may take a moment'
MSG_EN[build_ok]='build complete'
MSG_EN[build_fail]='build failed'
MSG_EN[cached]='cached ✓'
MSG_EN[linking]='linking…'
MSG_EN[skipped_word]=skipped
MSG_EN[no_write]='cannot write to'
MSG_EN[config_found]='existing config found at'
MSG_EN[config_overwrite_q]='Overwrite?'
MSG_EN[config_overwrite_hint]='[y/N]'
MSG_EN[config_keep]='will keep existing'
MSG_EN[config_will_overwrite]='will overwrite on install'
MSG_EN[config_skip]='config skipped'
MSG_EN[done_title]='Done — Maverick installed'
MSG_EN[done_hint]='Select '"'"'maverick'"'"' in your display manager, or run:'
MSG_EN[tip]=tip
MSG_EN[tip_text]='maverick --check-config validates your config without starting the WM'
MSG_EN[s_binaries]=binaries
MSG_EN[s_path]=PATH
MSG_EN[s_session]=session
MSG_EN[s_config]=config
MSG_EN[s_time]='time'
MSG_EN[installed]=installed
MSG_EN[not_in_path]='not in PATH — add:'
MSG_EN[path_ok]='already on PATH'
MSG_EN[path_q]='Add to PATH in'
MSG_EN[path_added]='PATH added to'
MSG_EN[path_configured]='already configured in'
MSG_EN[path_restart]='open a new terminal for it to take effect'
MSG_EN[path_write_fail]='could not add to PATH'
MSG_EN[checks_ok]='all systems go'
MSG_EN[ready_line]='cleared for takeoff'
MSG_EN[disk_space_warn]='low disk space (<2GB free)'
MSG_EN[verify_ok]='binary functional'
MSG_EN[verify_fail]='binary does not respond correctly'
MSG_EN[log_saved]='log saved to'
MSG_EN[wc_missing]='wc not found — alignment may be imprecise'
MSG_EN[err_cc]='no C linker (cc) — Arch: pacman -S base-devel · Debian: apt install build-essential · Fedora: dnf groupinstall '"'"'Development Tools'"'"''
MSG_EN[err_x11]='X11 client libraries not linkable (-lX11 -lX11-xcb) — Arch: pacman -S libx11 · Debian: apt install libx11-dev libxcb1-dev · Fedora: dnf install libX11-devel libxcb-devel'
MSG_EN[err_missing_exe]='missing executable'
MSG_EN[err_missing_exe_hint]='(run without --no-build)'
MSG_EN[err_stage_failed]='stage failed'
MSG_EN[err_staged_not_exec]='staged binary is not executable'
MSG_EN[err_is_dir]='is a directory; refusing to replace it'
MSG_EN[err_install_failed]='install failed'
MSG_EN[err_session_install]='session install failed'
MSG_EN[disk_unknown]='could not check disk space'
MSG_EN[disk_continue_q]='Continue anyway?'
MSG_EN[disk_continue_hint]='[y/N]'

declare -gA MSG_ES=()
MSG_ES[banner_sub]='gestor de ventanas columnar · tiling WM'
MSG_ES[detect_lang]='idioma detectado'
MSG_ES[os_detected]='sistema detectado'
MSG_ES[distro_detected]=distro
MSG_ES[dest]=destino
MSG_ES[confirm_q]='¿Instalar Maverick en'
MSG_ES[confirm_hint]='[S/n]'
MSG_ES[aborted]=cancelado.
MSG_ES[need_cargo]='cargo no está en el PATH — instala rustup desde https://rustup.rs'
MSG_ES[need_rust]='Rust no encontrado — se necesita cargo ≥ 1.82'
MSG_ES[install_rust_q]='¿Instalar Rust ahora?'
MSG_ES[install_rust_hint]='[s/N]'
MSG_ES[installing_rust]='instalando Rust…'
MSG_ES[rust_ok]='Rust instalado'
MSG_ES[rust_fail]='no se pudo instalar Rust automáticamente'
MSG_ES[unsupported]='sistema no soportado'
MSG_ES[use_linux]='Maverick es un window manager X11 solo para Linux. En Windows usa WSL2 con una distro Linux.'
MSG_ES[x11_only]='requiere Linux con X11 (X.Org / XLibre)'
MSG_ES[phase_deps]='Verificando dependencias'
MSG_ES[phase_build]=Compilando
MSG_ES[phase_install]='Instalando binarios'
MSG_ES[phase_session]='Sesión X11'
MSG_ES[phase_config]='Configuración'
MSG_ES[phase_final]='Verificación final'
MSG_ES[deps_ok]='todo en orden'
MSG_ES[building_detail]='puede tardar un momento'
MSG_ES[build_ok]='compilación completada'
MSG_ES[build_fail]='falló la compilación'
MSG_ES[cached]='caché ✓'
MSG_ES[linking]='enlazando…'
MSG_ES[skipped_word]=omitido
MSG_ES[no_write]='sin permiso de escritura en'
MSG_ES[config_found]='configuración existente en'
MSG_ES[config_overwrite_q]='¿Sobrescribir?'
MSG_ES[config_overwrite_hint]='[s/N]'
MSG_ES[config_keep]='se conservará la existente'
MSG_ES[config_will_overwrite]='se sobrescribirá al instalar'
MSG_ES[config_skip]='configuración omitida'
MSG_ES[done_title]='¡Listo! Maverick instalado'
MSG_ES[done_hint]='Selecciona «maverick» en tu gestor de sesión, o ejecuta:'
MSG_ES[tip]=consejo
MSG_ES[tip_text]='maverick --check-config valida tu config sin iniciar el WM'
MSG_ES[s_binaries]=binarios
MSG_ES[s_path]=PATH
MSG_ES[s_session]='sesión'
MSG_ES[s_config]=config
MSG_ES[s_time]=tiempo
MSG_ES[installed]=instalado
MSG_ES[not_in_path]='no está en PATH — añade:'
MSG_ES[path_ok]='ya en PATH'
MSG_ES[path_q]='¿Añadir a PATH en'
MSG_ES[path_added]='PATH añadido a'
MSG_ES[path_configured]='ya configurado en'
MSG_ES[path_restart]='abre una nueva terminal para que surta efecto'
MSG_ES[path_write_fail]='no se pudo añadir a PATH'
MSG_ES[checks_ok]='todos los sistemas listos'
MSG_ES[ready_line]='listo para el despegue'
MSG_ES[disk_space_warn]='espacio en disco bajo (<2GB libres)'
MSG_ES[verify_ok]='binario funcional'
MSG_ES[verify_fail]='el binario no responde correctamente'
MSG_ES[log_saved]='log guardado en'
MSG_ES[wc_missing]='wc no encontrado — alineación puede ser imprecisa'
MSG_ES[err_cc]='falta el enlazador C (cc) — Arch: pacman -S base-devel · Debian: apt install build-essential · Fedora: dnf groupinstall '"'"'Development Tools'"'"''
MSG_ES[err_x11]='las bibliotecas cliente de X11 no se pueden enlazar (-lX11 -lX11-xcb) — Arch: pacman -S libx11 · Debian: apt install libx11-dev libxcb1-dev · Fedora: dnf install libX11-devel libxcb-devel'
MSG_ES[err_missing_exe]='ejecutable ausente'
MSG_ES[err_missing_exe_hint]='(ejecuta sin --no-build)'
MSG_ES[err_stage_failed]='falló la preparación'
MSG_ES[err_staged_not_exec]='el binario preparado no es ejecutable'
MSG_ES[err_is_dir]='es un directorio; se rechaza reemplazarlo'
MSG_ES[err_install_failed]='falló la instalación'
MSG_ES[err_session_install]='falló la instalación de la sesión'
MSG_ES[disk_unknown]='no se pudo comprobar el espacio en disco'
MSG_ES[disk_continue_q]='¿Continuar de todos modos?'
MSG_ES[disk_continue_hint]='[s/N]'

# ── lookup ───────────────────────────────────────────────────────────────────
# An unknown key prints itself: a typo shows up as a visible word rather than
# an empty line, and the audit above turns the same case into a failing test.
t() {
    local k="${1:-}"
    if [[ "$LANG_ID" == "es" ]]; then
        printf '%s\n' "${MSG_ES[$k]-$k}"
    else
        printf '%s\n' "${MSG_EN[$k]-$k}"
    fi
    return 0
}

# ── audit ────────────────────────────────────────────────────────────────────
i18n_audit() {
    local rc=0 k src here used=" " unused=""
    for k in "${!MSG_EN[@]}"; do
        if [[ -z "${MSG_ES[$k]-}" ]]; then
            printf 'i18n: key present in en only: %s\n' "$k" >&2; rc=1
        fi
    done
    for k in "${!MSG_ES[@]}"; do
        if [[ -z "${MSG_EN[$k]-}" ]]; then
            printf 'i18n: key present in es only: %s\n' "$k" >&2; rc=1
        fi
    done
    here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
    for src in "$here/install.sh" "$here"/lib/*.sh; do
        [[ -f "$src" ]] || continue
        [[ "$src" == "${BASH_SOURCE[0]}" ]] && continue
        while IFS= read -r k; do
            [[ -n "$k" ]] || continue
            if [[ -z "${MSG_EN[$k]-}" ]]; then
                printf 'i18n: key used but never defined: %s (%s)\n' "$k" "${src##*/}" >&2
                rc=1
            else
                used+="$k "
            fi
        done < <(grep -oE '\$\(t [a-z_][a-z0-9_]*\)' "$src" 2>/dev/null |
                    sed -E 's/^\$\(t ([a-z_][a-z0-9_]*)\)$/\1/' | sort -u)
    done
    for k in "${!MSG_EN[@]}"; do
        [[ "$used" == *" $k "* ]] || unused+=" $k"
    done
    if [[ -n "$unused" ]]; then
        printf 'i18n: defined but never used:%s\n' "$unused"
    fi
    if [[ $rc -eq 0 ]]; then
        printf 'i18n: %d keys · en/es in parity · every key used is defined\n' "${#MSG_EN[@]}"
    fi
    return "$rc"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    i18n_audit
fi
