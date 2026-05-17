#!/usr/bin/env bash
set -euo pipefail

REPO="sandseal/sandseal"
INSTALL_DIR="${SANDSEAL_INSTALL_DIR:-$HOME/.local/bin}"
DATA_DIR="${SANDSEAL_DIR:-$HOME/.sandseal}"

info() { printf '\033[0;34m%s\033[0m\n' "$1"; }
warn() { printf '\033[0;33m%s\033[0m\n' "$1"; }
error() { printf '\033[0;31mError: %s\033[0m\n' "$1" >&2; exit 1; }

detect_os() {
    local os
    os="$(uname -s)"
    case "${os}" in
        Linux)  echo "linux" ;;
        Darwin) echo "darwin" ;;
        *)      error "unsupported OS: ${os}" ;;
    esac
}

detect_arch() {
    local arch
    arch="$(uname -m)"
    case "${arch}" in
        x86_64|amd64)   echo "x86_64" ;;
        aarch64|arm64)  echo "aarch64" ;;
        *)              error "unsupported architecture: ${arch}" ;;
    esac
}

get_latest_version() {
    if command -v curl &>/dev/null; then
        curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
            | grep '"tag_name"' | head -1 | sed 's/.*"v\(.*\)".*/\1/'
    elif command -v wget &>/dev/null; then
        wget -qO- "https://api.github.com/repos/${REPO}/releases/latest" \
            | grep '"tag_name"' | head -1 | sed 's/.*"v\(.*\)".*/\1/'
    else
        error "curl or wget required"
    fi
}

download() {
    local url="$1" dest="$2"
    if command -v curl &>/dev/null; then
        curl -fsSL -o "${dest}" "${url}"
    else
        wget -qO "${dest}" "${url}"
    fi
}

check_docker() {
    if ! command -v docker &>/dev/null; then
        warn "Docker not found. sandseal requires Docker to run sandboxes."
        warn "Install Docker: https://docs.docker.com/get-docker/"
    fi
}

main() {
    local os arch version target url tmpdir

    os="$(detect_os)"
    arch="$(detect_arch)"
    version="${SANDSEAL_VERSION:-$(get_latest_version)}"

    if [[ -z "${version}" ]]; then
        error "could not determine latest version"
    fi

    target="sandseal-${os}-${arch}"
    url="https://github.com/${REPO}/releases/download/v${version}/${target}.tar.gz"

    info "Installing sandseal v${version} (${os}/${arch})"

    tmpdir="$(mktemp -d)"
    trap 'rm -rf "${tmpdir}"' EXIT

    info "Downloading ${url}"
    download "${url}" "${tmpdir}/sandseal.tar.gz"

    tar -xzf "${tmpdir}/sandseal.tar.gz" -C "${tmpdir}"

    # Install binary
    mkdir -p "${INSTALL_DIR}"
    mv "${tmpdir}/sandseal" "${INSTALL_DIR}/sandseal"
    chmod +x "${INSTALL_DIR}/sandseal"
    info "Binary installed to ${INSTALL_DIR}/sandseal"

    # Install agents (Dockerfile, entrypoint, compose template)
    if [[ -d "${tmpdir}/agents" ]]; then
        mkdir -p "${DATA_DIR}/agents"
        cp -r "${tmpdir}/agents/." "${DATA_DIR}/agents/"
        chmod +x "${DATA_DIR}/agents/entrypoint.sh" 2>/dev/null || true
        chmod +x "${DATA_DIR}/agents/apt-wrapper.sh" 2>/dev/null || true
        info "Agents installed to ${DATA_DIR}/agents/"
    fi

    # Install schema
    if [[ -d "${tmpdir}/schema" ]]; then
        mkdir -p "${DATA_DIR}/schema"
        cp -r "${tmpdir}/schema/." "${DATA_DIR}/schema/"
        info "Schema installed to ${DATA_DIR}/schema/"
    fi

    # Check prerequisites
    check_docker

    # PATH hint
    if ! echo "${PATH}" | tr ':' '\n' | grep -qx "${INSTALL_DIR}"; then
        info ""
        info "Add ${INSTALL_DIR} to your PATH:"
        info "  export PATH=\"${INSTALL_DIR}:\$PATH\""
        info ""
        info "Or add to your shell config:"
        local shell_rc=""
        case "${SHELL:-/bin/bash}" in
            */zsh)  shell_rc="~/.zshrc" ;;
            */fish) shell_rc="~/.config/fish/config.fish" ;;
            *)      shell_rc="~/.bashrc" ;;
        esac
        info "  echo 'export PATH=\"${INSTALL_DIR}:\$PATH\"' >> ${shell_rc}"
    fi

    info ""
    info "Run 'sandseal --help' to get started."
}

main "$@"
