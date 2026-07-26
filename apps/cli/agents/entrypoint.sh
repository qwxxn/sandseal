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

# Install bundled skills into the agent home. Copied rather than symlinked so the agent can
# read them even though /opt is read-only, and refreshed every start so a CLI upgrade ships
# updated skills without touching the persistent volume by hand.
if [[ -d /opt/sandseal/skills ]]; then
  mkdir -p "${HOME}/.claude/skills"
  cp -r /opt/sandseal/skills/. "${HOME}/.claude/skills/"
fi

echo "Starting agent CLI..." >&2

exec "$@"
