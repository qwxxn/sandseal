#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
INSTALL_DIR="${SANDSEAL_INSTALL_DIR:-$HOME/.local/bin}"
DATA_DIR="${SANDSEAL_DIR:-$HOME/.sandseal}"

info() { printf '\033[0;34m%s\033[0m\n' "$1"; }
error() { printf '\033[0;31mError: %s\033[0m\n' "$1" >&2; exit 1; }

main() {
    info "Building sandseal (release)..."
    cd "${REPO_ROOT}/cli"
    cargo build --release

    local binary="${REPO_ROOT}/cli/target/release/sandseal"
    if [[ ! -f "${binary}" ]]; then
        error "build failed — binary not found at ${binary}"
    fi

    # Install binary atomically: copy to a temp file in the same dir, then
    # rename over the target. A plain cp truncates the destination in place,
    # which fails with ETXTBSY ("Text file busy") if a sandseal is currently
    # running; rename(2) just swaps the dir entry and leaves the running
    # process on its old inode.
    mkdir -p "${INSTALL_DIR}"
    local tmp="${INSTALL_DIR}/.sandseal.new.$$"
    cp "${binary}" "${tmp}"
    chmod +x "${tmp}"
    mv -f "${tmp}" "${INSTALL_DIR}/sandseal"
    info "Binary → ${INSTALL_DIR}/sandseal"

    # Install agents
    mkdir -p "${DATA_DIR}/agents"
    cp -r "${REPO_ROOT}/cli/agents/." "${DATA_DIR}/agents/"
    chmod +x "${DATA_DIR}/agents/entrypoint.sh" 2>/dev/null || true
    chmod +x "${DATA_DIR}/agents/apt-wrapper.sh" 2>/dev/null || true
    info "Agents → ${DATA_DIR}/agents/"

    # Install schema
    if [[ -d "${REPO_ROOT}/cli/schema" ]]; then
        mkdir -p "${DATA_DIR}/schema"
        cp -r "${REPO_ROOT}/cli/schema/." "${DATA_DIR}/schema/"
        info "Schema → ${DATA_DIR}/schema/"
    fi

    info ""
    info "Done. Version: $(${INSTALL_DIR}/sandseal --version 2>/dev/null || echo 'unknown')"

    if ! echo "${PATH}" | tr ':' '\n' | grep -qx "${INSTALL_DIR}"; then
        info "Note: ${INSTALL_DIR} is not in PATH"
    fi
}

main "$@"
