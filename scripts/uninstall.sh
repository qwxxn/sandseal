#!/usr/bin/env bash
set -euo pipefail

INSTALL_DIR="${SANDSEAL_INSTALL_DIR:-$HOME/.local/bin}"
DATA_DIR="${SANDSEAL_DIR:-$HOME/.sandseal}"

info() { printf '\033[0;34m%s\033[0m\n' "$1"; }
warn() { printf '\033[0;33m%s\033[0m\n' "$1"; }

confirm() {
    local prompt="$1"
    printf '\033[0;33m%s [y/N] \033[0m' "${prompt}"
    read -r answer
    [[ "${answer}" =~ ^[Yy] ]]
}

main() {
    info "Sandseal uninstaller"
    echo ""

    # Remove binary
    local binary="${INSTALL_DIR}/sandseal"
    if [[ -f "${binary}" ]]; then
        rm -f "${binary}"
        info "Removed ${binary}"
    else
        warn "Binary not found at ${binary}"
    fi

    # Remove data directory (agents, schema, tmp, auth)
    if [[ -d "${DATA_DIR}" ]]; then
        if confirm "Remove config and data directory (${DATA_DIR})?"; then
            rm -rf "${DATA_DIR}"
            info "Removed ${DATA_DIR}"
        else
            info "Kept ${DATA_DIR}"
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
