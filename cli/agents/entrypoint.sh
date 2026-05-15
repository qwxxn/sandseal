#!/bin/bash
set -euo pipefail

TTYD_PORT="${TTYD_PORT:-7681}"

# Execute prestart hook scripts (if any are mounted)
if [[ -d /tmp/prestart-scripts ]]; then
  for script in /tmp/prestart-scripts/*; do
    [[ -f "${script}" ]] || continue
    [[ "${script}" == *.gitkeep ]] && continue
    echo "Running prestart hook: $(basename "${script}")..." >&2
    "${script}"
  done
fi

echo "Starting agent CLI..." >&2
echo "Web terminal available at http://localhost:${TTYD_PORT}" >&2

# Start Claude Code inside a tmux session
tmux new-session -d -s main "$@"
tmux set-option -g mouse on

# Start ttyd in background (connects to the same tmux session)
setsid ttyd --port "${TTYD_PORT}" --writable tmux attach -t main >/dev/null 2>&1 &

# Attach to tmux in foreground (docker attach sees this)
exec tmux attach -t main
