#!/bin/bash
# Wrapper around apt-get that logs runtime-installed packages.
# Placed at /usr/local/bin/apt-get to intercept calls.

REAL_APT="/usr/bin/apt-get"
LOG_FILE="${SANDSEAL_RUNTIME_PACKAGES:-/tmp/.sandseal-runtime-packages}"

if [[ "$1" == "install" ]]; then
  shift
  packages=()
  for arg in "$@"; do
    # Skip flags
    [[ "$arg" == -* ]] && continue
    packages+=("$arg")
  done

  if [[ ${#packages[@]} -gt 0 ]]; then
    printf '%s\n' "${packages[@]}" >> "$LOG_FILE"
  fi

  exec "$REAL_APT" install "$@"
else
  exec "$REAL_APT" "$@"
fi
