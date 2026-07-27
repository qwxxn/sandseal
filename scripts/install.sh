#!/usr/bin/env bash
set -euo pipefail

# Sandseal installer.
#
#   curl -fsSL https://sandseal.io/install.sh | bash
#
# Downloads the release binary for this platform, installs the sandbox assets,
# puts the binary on PATH and makes sure Docker is usable. Safe to re-run: it
# upgrades in place.
#
# Flags (also settable as env vars):
#   --version X.Y.Z     SANDSEAL_VERSION       install a specific version
#   --dir PATH          SANDSEAL_INSTALL_DIR   where the binary goes
#   --data-dir PATH     SANDSEAL_DIR           where agents/ and schema/ go
#   --no-modify-path    SANDSEAL_NO_MODIFY_PATH=1
#   --with-docker       SANDSEAL_INSTALL_DOCKER=1   install Docker without asking
#   --no-docker         SANDSEAL_INSTALL_DOCKER=0   never install Docker
#   --no-verify         SANDSEAL_NO_VERIFY=1   skip the SHA256 check
#   --yes               SANDSEAL_ASSUME_YES=1  answer every prompt with yes

REPO="${SANDSEAL_REPO:-qwxxn/sandseal}"
INSTALL_DIR="${SANDSEAL_INSTALL_DIR:-$HOME/.local/bin}"
DATA_DIR="${SANDSEAL_DIR:-$HOME/.sandseal}"
VERSION="${SANDSEAL_VERSION:-}"
# Directory URL holding the release assets. Overridable for mirrors and for
# testing the installer against a locally built tarball.
ASSET_BASE="${SANDSEAL_ASSET_BASE:-}"
MODIFY_PATH=1; [[ "${SANDSEAL_NO_MODIFY_PATH:-0}" == "1" ]] && MODIFY_PATH=0
VERIFY=1;      [[ "${SANDSEAL_NO_VERIFY:-0}" == "1" ]] && VERIFY=0
ASSUME_YES=0;  [[ "${SANDSEAL_ASSUME_YES:-0}" == "1" ]] && ASSUME_YES=1
INSTALL_DOCKER="${SANDSEAL_INSTALL_DOCKER:-ask}"

if [[ -t 1 ]]; then
    C_INFO=$'\033[0;34m'; C_OK=$'\033[0;32m'; C_WARN=$'\033[0;33m'; C_ERR=$'\033[0;31m'; C_OFF=$'\033[0m'
else
    C_INFO=""; C_OK=""; C_WARN=""; C_ERR=""; C_OFF=""
fi

info() { printf '%s%s%s\n' "${C_INFO}" "$1" "${C_OFF}"; }
ok()   { printf '%s%s%s\n' "${C_OK}" "$1" "${C_OFF}"; }
warn() { printf '%s%s%s\n' "${C_WARN}" "$1" "${C_OFF}" >&2; }
error() { printf '%sError: %s%s\n' "${C_ERR}" "$1" "${C_OFF}" >&2; exit 1; }
have() { command -v "$1" &>/dev/null; }

usage() {
    cat <<'USAGE'
Sandseal installer.

  curl -fsSL https://sandseal.io/install.sh | bash

  --version X.Y.Z     install a specific version        (SANDSEAL_VERSION)
  --dir PATH          where the binary goes             (SANDSEAL_INSTALL_DIR)
  --data-dir PATH     where agents/ and schema/ go      (SANDSEAL_DIR)
  --no-modify-path    do not touch shell rc files       (SANDSEAL_NO_MODIFY_PATH=1)
  --with-docker       install Docker without asking     (SANDSEAL_INSTALL_DOCKER=1)
  --no-docker         never install Docker              (SANDSEAL_INSTALL_DOCKER=0)
  --no-verify         skip the SHA256 check             (SANDSEAL_NO_VERIFY=1)
  --yes, -y           answer every prompt with yes      (SANDSEAL_ASSUME_YES=1)
USAGE
    exit 0
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --version)  VERSION="${2:?--version needs a value}"; shift 2 ;;
        --dir)      INSTALL_DIR="${2:?--dir needs a value}"; shift 2 ;;
        --data-dir) DATA_DIR="${2:?--data-dir needs a value}"; shift 2 ;;
        --no-modify-path) MODIFY_PATH=0; shift ;;
        --with-docker)    INSTALL_DOCKER=1; shift ;;
        --no-docker)      INSTALL_DOCKER=0; shift ;;
        --no-verify)      VERIFY=0; shift ;;
        --yes|-y)         ASSUME_YES=1; shift ;;
        --help|-h)        usage ;;
        *) error "unknown option: $1 (try --help)" ;;
    esac
done

# The script is usually piped into bash, so stdin is the script itself — prompts
# have to come from the terminal or not at all.
confirm() {
    [[ ${ASSUME_YES} -eq 1 ]] && return 0
    # -r /dev/tty is true even with no controlling terminal; only opening it tells.
    { : < /dev/tty; } 2>/dev/null || return 1
    local answer=""
    printf '%s%s [Y/n] %s' "${C_WARN}" "$1" "${C_OFF}" > /dev/tty
    read -r answer < /dev/tty || return 1
    [[ -z "${answer}" || "${answer}" =~ ^[Yy] ]]
}

detect_os() {
    case "$(uname -s)" in
        Linux)  echo "linux" ;;
        Darwin) echo "darwin" ;;
        MINGW*|MSYS*|CYGWIN*)
            error "native Windows is not supported — install sandseal inside WSL2" ;;
        *) error "unsupported OS: $(uname -s)" ;;
    esac
}

detect_arch() {
    case "$(uname -m)" in
        x86_64|amd64)  echo "x86_64" ;;
        aarch64|arm64) echo "aarch64" ;;
        *) error "unsupported architecture: $(uname -m)" ;;
    esac
}

fetch() {  # url dest
    if have curl; then curl -fsSL -o "$2" "$1"
    else wget -qO "$2" "$1"; fi
}

# Resolve the newest tag from the /releases/latest redirect rather than the API:
# api.github.com is rate limited per IP (60/hour), which is exactly the kind of
# failure a one-line installer must not have.
resolve_latest() {
    local url=""
    if have curl; then
        url="$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
            "https://github.com/${REPO}/releases/latest" 2>/dev/null || true)"
    else
        url="$(wget --spider --server-response --max-redirect=5 \
            "https://github.com/${REPO}/releases/latest" 2>&1 \
            | awk '/[Ll]ocation: /{print $2}' | tail -1 || true)"
    fi
    [[ "${url}" =~ /tag/v?([0-9][^/[:space:]]*) ]] && printf '%s' "${BASH_REMATCH[1]}"
}

sha256_of() {
    if have sha256sum; then sha256sum "$1" | cut -d' ' -f1
    elif have shasum; then shasum -a 256 "$1" | cut -d' ' -f1
    else return 1; fi
}

verify_checksum() {  # file sums_url name
    local file="$1" sums_url="$2" name="$3" sums="" want="" got=""
    sums="$(dirname "${file}")/SHA256SUMS"
    if ! fetch "${sums_url}" "${sums}" 2>/dev/null; then
        error "could not download ${sums_url} — refusing to install unverified binary (--no-verify overrides)"
    fi
    want="$(grep -F " ${name}" "${sums}" | head -1 | cut -d' ' -f1)"
    [[ -n "${want}" ]] || error "${name} is missing from SHA256SUMS"
    got="$(sha256_of "${file}")" || { warn "no sha256sum/shasum available — skipping verification"; return 0; }
    [[ "${got}" == "${want}" ]] || error "checksum mismatch for ${name} (expected ${want}, got ${got})"
    ok "Checksum verified"
}

# Replace a directory we own wholesale instead of merging into it: a copy on top
# of the old contents leaves files that a newer release deleted, and the sandbox
# then builds against a mix of two versions.
#
# The old tree is moved aside, never deleted. This directory is installer-owned,
# but it is also where a hand-edited Dockerfile or an own skill would sit, and
# losing that silently to an upgrade is not a trade this script gets to make.
replace_dir() {  # src dest
    local src="$1" dest="$2" tmp="${2}.new.$$" extra=""
    rm -rf "${tmp}"
    mkdir -p "$(dirname "${dest}")"
    cp -R "${src}" "${tmp}"

    if [[ -d "${dest}" ]]; then
        extra="$( (cd "${dest}" && find . -type f | sort) \
            | comm -23 - <(cd "${tmp}" && find . -type f | sort) )"
        rm -rf "${dest}.previous"
        mv "${dest}" "${dest}.previous"
        if [[ -n "${extra}" ]]; then
            warn "These are not part of the release and did not survive the upgrade:"
            printf '%s\n' "${extra}" | sed "s|^\./|  ${dest}/|" >&2
            warn "The whole previous directory is at ${dest}.previous"
        fi
    fi

    mv "${tmp}" "${dest}"
}

path_snippet() {  # shell -> line that puts INSTALL_DIR on PATH
    case "$1" in
        fish) printf 'fish_add_path %s\n' "${INSTALL_DIR}" ;;
        *)    printf 'export PATH="%s:$PATH"\n' "${INSTALL_DIR}" ;;
    esac
}

ensure_path() {
    case ":${PATH}:" in *":${INSTALL_DIR}:"*) return 0 ;; esac

    if [[ ${MODIFY_PATH} -eq 0 ]]; then
        warn "${INSTALL_DIR} is not on PATH. Add it manually:"
        warn "  $(path_snippet bash)"
        return 0
    fi

    local marker="# added by the sandseal installer"
    local rcfiles=() touched=()

    [[ -f "${HOME}/.bashrc" || "${SHELL:-}" == */bash ]] && rcfiles+=("${HOME}/.bashrc")
    [[ -f "${HOME}/.zshrc"  || "${SHELL:-}" == */zsh  ]] && rcfiles+=("${HOME}/.zshrc")
    # A macOS Terminal starts a *login* shell, which reads .bash_profile/.zprofile
    # and never touches the rc file — skipping these leaves PATH unset there.
    [[ -f "${HOME}/.bash_profile" ]] && rcfiles+=("${HOME}/.bash_profile")
    [[ -f "${HOME}/.zprofile" ]] && rcfiles+=("${HOME}/.zprofile")
    # Always, even when absent: a login shell that finds no .profile reads nothing
    # else either, and PATH would silently be missing there.
    rcfiles+=("${HOME}/.profile")
    if [[ -d "${HOME}/.config/fish" || "${SHELL:-}" == */fish ]]; then
        rcfiles+=("${HOME}/.config/fish/config.fish")
    fi
    [[ ${#rcfiles[@]} -eq 0 ]] && rcfiles=("${HOME}/.profile")

    local rc
    for rc in "${rcfiles[@]}"; do
        [[ -e "${rc}" ]] || { mkdir -p "$(dirname "${rc}")"; : > "${rc}"; }
        grep -qF "${marker}" "${rc}" 2>/dev/null && continue
        local shell_kind="sh"
        [[ "${rc}" == *fish* ]] && shell_kind="fish"
        printf '\n%s\n%s\n' "${marker}" "$(path_snippet "${shell_kind}")" >> "${rc}"
        touched+=("${rc}")
    done

    if [[ ${#touched[@]} -gt 0 ]]; then
        ok "PATH updated in: ${touched[*]}"
        NEEDS_RELOAD=1
    fi
}

install_docker_linux() {
    have curl || { warn "curl is required to install Docker automatically"; return 1; }
    info "Installing Docker via https://get.docker.com ..."
    if [[ $(id -u) -eq 0 ]]; then
        curl -fsSL https://get.docker.com | sh
    elif have sudo; then
        curl -fsSL https://get.docker.com | sudo sh
    else
        warn "need root or sudo to install Docker"; return 1
    fi
    if have sudo && [[ $(id -u) -ne 0 ]]; then
        sudo usermod -aG docker "$(id -un)" 2>/dev/null || true
        warn "You were added to the 'docker' group — log out and back in for it to take effect."
    fi
}

check_docker() {
    if have docker; then
        if docker info &>/dev/null; then
            ok "Docker is ready"
        else
            warn "Docker is installed but the daemon is not reachable."
            if [[ "$(uname -s)" == "Darwin" ]]; then
                warn "  Start Docker Desktop, then run: sandseal start ."
            elif grep -qi microsoft /proc/version 2>/dev/null; then
                warn "  On WSL: start Docker Desktop and enable WSL integration for this distro."
            else
                warn "  Try: sudo systemctl start docker"
                warn "  If it says 'permission denied', run: sudo usermod -aG docker \$USER  (then re-login)"
            fi
        fi
        return 0
    fi

    warn "Docker not found — sandseal needs it to run sandboxes."

    if [[ "$(uname -s)" == "Darwin" ]]; then
        warn "  Install Docker Desktop: https://docs.docker.com/desktop/install/mac-install/"
        have brew && warn "  or: brew install --cask docker"
        return 0
    fi
    if grep -qi microsoft /proc/version 2>/dev/null; then
        warn "  On WSL, install Docker Desktop on Windows and enable WSL integration:"
        warn "  https://docs.docker.com/desktop/wsl/"
        return 0
    fi

    case "${INSTALL_DOCKER}" in
        1) install_docker_linux || warn "Install it manually: https://docs.docker.com/engine/install/" ;;
        0) warn "  Install it with: curl -fsSL https://get.docker.com | sh" ;;
        *)
            if confirm "Install Docker now (runs https://get.docker.com as root)?"; then
                install_docker_linux || warn "Install it manually: https://docs.docker.com/engine/install/"
            else
                warn "  Install it later with: curl -fsSL https://get.docker.com | sh"
            fi
            ;;
    esac
}

main() {
    local os arch target url
    # Not local: the EXIT trap runs in the global scope after main returns, and
    # under `set -u` a local name there is an unbound-variable error.
    WORK_DIR=""
    NEEDS_RELOAD=0

    have tar || error "tar is required"
    have curl || have wget || error "curl or wget is required"

    os="$(detect_os)"
    arch="$(detect_arch)"

    if [[ -z "${VERSION}" ]]; then
        # `|| true`: without it a repo that has no release yet makes the command
        # substitution fail, and errexit kills the script before the message below.
        VERSION="$(resolve_latest || true)"
        [[ -n "${VERSION}" ]] || error "no published release found for ${REPO} — pass --version or build from source"
    fi
    VERSION="${VERSION#v}"

    target="sandseal-${os}-${arch}"
    : "${ASSET_BASE:=https://github.com/${REPO}/releases/download/v${VERSION}}"
    url="${ASSET_BASE}/${target}.tar.gz"

    info "Installing sandseal v${VERSION} (${os}/${arch})"

    WORK_DIR="$(mktemp -d)"
    trap 'rm -rf "${WORK_DIR}"' EXIT

    fetch "${url}" "${WORK_DIR}/${target}.tar.gz" \
        || error "download failed: ${url}"

    if [[ ${VERIFY} -eq 1 ]]; then
        verify_checksum "${WORK_DIR}/${target}.tar.gz" "${ASSET_BASE}/SHA256SUMS" "${target}.tar.gz"
    fi

    tar -xzf "${WORK_DIR}/${target}.tar.gz" -C "${WORK_DIR}" 2>/dev/null \
        || error "could not unpack ${target}.tar.gz — the download is corrupt, try again"
    [[ -f "${WORK_DIR}/sandseal" ]] || error "archive does not contain the sandseal binary"
    chmod +x "${WORK_DIR}/sandseal"

    # Install the binary by rename, not by overwrite: cp truncates the target in
    # place and fails with ETXTBSY while an older sandseal is still running.
    mkdir -p "${INSTALL_DIR}"
    cp "${WORK_DIR}/sandseal" "${INSTALL_DIR}/.sandseal.new.$$"
    chmod +x "${INSTALL_DIR}/.sandseal.new.$$"
    mv -f "${INSTALL_DIR}/.sandseal.new.$$" "${INSTALL_DIR}/sandseal"
    ok "Binary → ${INSTALL_DIR}/sandseal"

    if [[ -d "${WORK_DIR}/agents" ]]; then
        replace_dir "${WORK_DIR}/agents" "${DATA_DIR}/agents"
        chmod +x "${DATA_DIR}/agents/entrypoint.sh" 2>/dev/null || true
        chmod +x "${DATA_DIR}/agents/apt-wrapper.sh" 2>/dev/null || true
        ok "Agents → ${DATA_DIR}/agents/"
    else
        error "archive does not contain agents/ — the sandbox cannot be built without it"
    fi

    if [[ -d "${WORK_DIR}/schema" ]]; then
        replace_dir "${WORK_DIR}/schema" "${DATA_DIR}/schema"
        ok "Schema → ${DATA_DIR}/schema/"
    fi

    ensure_path
    check_docker

    local reported
    reported="$("${INSTALL_DIR}/sandseal" --version 2>/dev/null || echo "unknown")"
    echo ""
    ok "Installed: ${reported}"

    if [[ ${NEEDS_RELOAD} -eq 1 ]]; then
        info "Open a new terminal, or run this once in the current one:"
        info "  export PATH=\"${INSTALL_DIR}:\$PATH\""
    fi
    info "Then start a sandbox:  sandseal start ."
}

main "$@"
