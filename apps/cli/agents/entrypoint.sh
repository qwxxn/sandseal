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

# Register the memory MCP server and the recall hook in the agent's own config. Done here
# rather than on the host because the agent home is a Docker volume, and done every start so
# a mounted ~/.claude.json (a common way to carry a login in) is merged, never replaced.
if [[ -n "${SANDSEAL_MEMORY_TOKEN:-}" ]] && command -v sandseal >/dev/null 2>&1; then
  sandseal memory provision || echo "warning: memory provisioning failed, continuing without it" >&2
fi

echo "Starting agent CLI..." >&2

exec "$@"
