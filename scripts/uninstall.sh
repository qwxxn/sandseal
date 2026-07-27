#!/usr/bin/env bash
set -euo pipefail

INSTALL_DIR="${SANDSEAL_INSTALL_DIR:-$HOME/.local/bin}"
DATA_DIR="${SANDSEAL_DIR:-$HOME/.sandseal}"
# Mirrors Rust's dirs::config_dir(), which is where the CLI keeps auth.json.
if [[ "$(uname -s)" == "Darwin" ]]; then
    CONFIG_DIR="${HOME}/Library/Application Support/sandseal"
else
    CONFIG_DIR="${XDG_CONFIG_HOME:-${HOME}/.config}/sandseal"
fi
ASSUME_YES=0; [[ "${SANDSEAL_ASSUME_YES:-0}" == "1" ]] && ASSUME_YES=1

while [[ $# -gt 0 ]]; do
    case "$1" in
        --yes|-y) ASSUME_YES=1; shift ;;
        --help|-h) printf 'Usage: uninstall.sh [--yes]\n'; exit 0 ;;
        *) printf 'Unknown option: %s\n' "$1" >&2; exit 1 ;;
    esac
done

info() { printf '\033[0;34m%s\033[0m\n' "$1"; }
warn() { printf '\033[0;33m%s\033[0m\n' "$1"; }

# Read from the terminal, not stdin: the script is meant to be piped into bash
# (`curl … | bash`), where stdin is the script itself and a plain `read` would
# swallow the rest of it.
have_tty() { { : < /dev/tty; } 2>/dev/null; }

confirm() {
    [[ ${ASSUME_YES} -eq 1 ]] && return 0
    # -r /dev/tty is true even with no controlling terminal; only opening it tells.
    have_tty || return 1
    local answer=""
    printf '\033[0;33m%s [y/N] \033[0m' "$1" > /dev/tty
    read -r answer < /dev/tty || return 1
    [[ "${answer}" =~ ^[Yy] ]]
}

# Undo what install.sh appended to the shell rc files: the marker line plus the
# export right after it.
strip_path_entries() {
    local marker="# added by the sandseal installer"
    local rc tmp
    for rc in "${HOME}/.bashrc" "${HOME}/.zshrc" "${HOME}/.bash_profile" \
              "${HOME}/.zprofile" "${HOME}/.profile" "${HOME}/.config/fish/config.fish"; do
        [[ -f "${rc}" ]] || continue
        grep -qF "${marker}" "${rc}" 2>/dev/null || continue
        tmp="${rc}.sandseal-uninstall.$$"
        awk -v m="${marker}" '
            index($0, m) { skip = 2 }
            skip > 0     { skip--; next }
                         { print }
        ' "${rc}" > "${tmp}" && mv "${tmp}" "${rc}"
        info "Removed PATH entry from ${rc}"
    done
}

main() {
    info "Sandseal uninstaller"
    if [[ ${ASSUME_YES} -eq 0 ]] && ! have_tty; then
        warn "No terminal to ask on — the binary goes, config and Docker leftovers stay."
        warn "Re-run with --yes to remove everything."
    fi
    echo ""

    # Remove binary
    local binary="${INSTALL_DIR}/sandseal"
    if [[ -f "${binary}" ]]; then
        rm -f "${binary}"
        info "Removed ${binary}"
    else
        warn "Binary not found at ${binary}"
    fi

    strip_path_entries

    # Remove data and config. The login token is NOT in DATA_DIR: auth/token.rs
    # puts it under dirs::config_dir(), so removing only ~/.sandseal left a live
    # credential on disk after an "uninstall".
    local -a dirs=()
    [[ -d "${DATA_DIR}" ]] && dirs+=("${DATA_DIR}")
    [[ -d "${CONFIG_DIR}" ]] && dirs+=("${CONFIG_DIR}")
    if [[ ${#dirs[@]} -gt 0 ]]; then
        if confirm "Remove config, data and the login token (${dirs[*]})?"; then
            local d
            for d in "${dirs[@]}"; do
                rm -rf "${d}"
                info "Removed ${d}"
            done
        else
            info "Kept ${dirs[*]}"
        fi
    fi

    # Docker cleanup
    if command -v docker &>/dev/null; then
        local containers
        containers="$(docker ps -a --filter 'label=sandseal.project_name' --format '{{.Names}}' 2>/dev/null || true)"

        if [[ -n "${containers}" ]]; then
            if confirm "Stop and remove running sandseal containers?"; then
                echo "${containers}" | while read -r name; do
                    docker rm -f "${name}" &>/dev/null || true
                done
                info "Removed sandseal containers"
            fi
        fi

        local images
        images="$(docker images --format '{{.Repository}}:{{.Tag}}' 'sandseal-sandbox/*' 2>/dev/null || true)"

        if [[ -n "${images}" ]]; then
            if confirm "Remove sandseal Docker images?"; then
                echo "${images}" | while read -r img; do
                    docker rmi "${img}" &>/dev/null || true
                done
                info "Removed sandseal images"
            fi
        fi

        local volumes
        volumes="$(docker volume ls --format '{{.Name}}' | grep '^sandseal-' 2>/dev/null || true)"

        if [[ -n "${volumes}" ]]; then
            if confirm "Remove sandseal Docker volumes (agent home, apt cache)?"; then
                echo "${volumes}" | while read -r vol; do
                    docker volume rm "${vol}" &>/dev/null || true
                done
                info "Removed sandseal volumes"
            fi
        fi
    fi

    echo ""
    info "Sandseal has been uninstalled."
}

main "$@"
