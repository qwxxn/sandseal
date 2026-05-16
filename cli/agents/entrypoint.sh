#!/bin/bash
set -euo pipefail

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

# Start Claude Code inside a tmux session
tmux new-session -d -s main "$@"
tmux set-option -g mouse on

# Attach to tmux in foreground (docker attach sees this)
exec tmux attach -t main
