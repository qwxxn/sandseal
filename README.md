# Sandseal

Isolated Docker sandboxes for AI coding agents.

Run Claude Code (and other AI agents) in a secure, containerized environment with fine-grained file access control, custom dependencies, and host networking — without touching your host system.

## Quick start

```bash
curl -fsSL https://raw.githubusercontent.com/sandseal/sandseal/main/scripts/install.sh | bash
```

Then in any project directory:

```bash
sandseal start .
```

This builds a sandbox image, mounts your project, and drops you into an isolated shell with the agent installed.

## Features

- **File access control** — hide secrets (`.env`, credentials) via `/dev/null` mounts, expose only what the agent needs
- **File inclusions** — mount additional host paths into the sandbox
- **Custom dependencies** — install APT packages at build time
- **Hooks** — run scripts at setup, prestart, and on the host before/after the sandbox
- **Host networking** — `network_mode: host` so the agent can reach your local services
- **Workspace mounts** — give the agent read-only (or read-write) access to other directories
- **Service endpoints** — map hostnames to host IPs for database access etc.
- **Docker passthrough** — optionally mount the Docker socket for agents that need it
- **Configuration profiles** — named settings presets (`night`, `review`, `untrusted`) switched per project with `sandseal config set`
- **Persistent agent home** — packages installed by the agent survive restarts
- **Debug mode** — drop into a bash shell instead of the agent CLI with `-d`
- **Concurrent instances** — run multiple sandboxes for the same project

## Configuration

Create `.sandseal/settings.json` in your project (or `~/.sandseal/settings.json` globally). Project settings are merged on top of global settings.

```json
{
  "$schema": "https://raw.githubusercontent.com/sandseal/sandseal/main/schema/settings.schema.json",
  "files": {
    "exclude": [".env", ".env.*", "secrets/"],
    "include": {
      "/home/user/.ssh/config": "/home/agent/.ssh/config"
    }
  },
  "dependencies": ["postgresql-client", "redis-tools"],
  "environment": {
    "DATABASE_URL": "postgres://localhost:5432/mydb"
  },
  "hooks": {
    "prestart": [{ "script": "npm install" }]
  },
  "container": {
    "memoryLimit": "8g"
  },
  "network": {
    "mode": "host"
  }
}
```

Full schema: [`schema/settings.schema.json`](schema/settings.schema.json)

## Profiles

A profile is a named settings file in `~/.sandseal/profiles/<name>.json`, using the same
schema as `settings.json`. It slots between the global and project layers, so you can
switch a whole set of restrictions on and off per project instead of editing one config.

```bash
sandseal config create night                # ~/.sandseal/profiles/night.json
sandseal config edit night                  # open in $EDITOR, validated on save
sandseal config set night                   # activate for this project
sandseal config list                        # available profiles, active one marked *
sandseal config effective                   # merged settings actually used
```

```json
{
  "$schema": "https://raw.githubusercontent.com/sandseal/sandseal/main/schema/settings.schema.json",
  "description": "unattended runs: no prod secrets, no host network, no docker socket",
  "$replace": ["environment", "files.include"],
  "environment": {},
  "files": { "exclude": [".env.production", "secrets/"] },
  "network": { "mode": "bridge" },
  "docker": { "passthrough": false }
}
```

### Layering

Layers are deep-merged, lowest precedence first:

```
~/.sandseal/settings.json  ->  <project>/.sandseal/settings.json  ->  profile
```

Global carries the machine-wide defaults, the project adds its own specifics, and the
profile lands on top — so a project cannot re-open what a profile closed.

| Value type | Merge behaviour |
|---|---|
| Scalar (`network.mode`) | Higher layer wins |
| Array (`files.exclude`) | Concatenated and deduplicated — exclusions only ever grow |
| Object (`environment`) | Merged key by key; a higher layer adds or overwrites keys |

### Removing inherited values

Merging alone can only add or overwrite keys, never remove one — so an empty
`"environment": {}` in a profile would leave the global secrets untouched. Declare the
paths a layer replaces wholesale:

```json
"$replace": ["environment", "files.include"]
```

Those paths are dropped from the lower layers before the merge, so the layer's own value
wins whole. Any layer may use it, not just profiles. Leave `files.exclude` out of
`$replace` — a profile should only ever hide more, never less.

Paths are dot-separated settings keys, **one per array item** — `["environment",
"files.include"]`, not `["environment, files.include"]`. Every entry is checked against
the schema and an unknown path is a hard error: a path that resolves to nothing would
silently disable the directive and leave the inherited values in place. A single key
inside a free-form map works (`environment.API_TOKEN`), but map keys that themselves
contain dots (the paths under `files.include`) are not addressable — replace the whole map.

The active profile is stored in `<project>/.sandseal/state.json` (add it to `.gitignore` —
it is machine-local). `sandseal config set --global` sets a machine-wide fallback used by
projects that have not picked one. A missing profile is a hard error, never a silent skip.

```
sandseal config list                      List profiles, mark the active one
sandseal config set <name>                Activate a profile for this project
sandseal config set <name> --global       Activate machine-wide (fallback)
sandseal config unset                     Clear the active profile
sandseal config show [name]               Print a profile (default: active)
sandseal config create <name> [--from x]  Create a profile, optionally by copying
sandseal config edit [name]               Open in $EDITOR and validate
sandseal config delete <name>             Delete a profile
sandseal config effective                 Print the merged settings
```

## CLI usage

```
sandseal start [path]        Start a sandbox (default: current directory)
sandseal start -d [path]     Start in debug mode (bash shell)
sandseal start --rebuild     Force rebuild the Docker image
sandseal start --profile x   Use profile x instead of the active one
sandseal start --no-profile  Ignore the active profile for this run
sandseal destroy [path]      Destroy sandbox for a project
sandseal destroy --all       Destroy all sandboxes
sandseal status              Show running sandboxes
sandseal config <cmd>        Manage configuration profiles (see above)
```

## How it works

Sandseal generates a Docker Compose configuration on the fly:

1. Builds a sandbox image (Ubuntu 24.04 + agent + your dependencies)
2. Mounts your project directory read-write
3. Hides excluded files via `/dev/null` bind mounts
4. Injects environment variables and runs hooks
5. Starts the agent CLI (or bash in debug mode)
6. Cleans up on exit (SIGINT/SIGTERM handled gracefully)

The agent runs as a non-root user with UID matching your host user, so file permissions work seamlessly.

## Building from source

```bash
cd cli
cargo build --release
```

The binary is at `cli/target/release/sandseal`.

## Project structure

```
sandseal/
├── cli/                  Rust CLI (cargo workspace)
│   ├── crates/
│   │   ├── sandseal/     Main binary
│   │   └── sandseal-protocol/  Shared types
│   └── Cargo.toml
├── agents/               Agent Dockerfiles and install scripts
│   ├── Dockerfile        Base sandbox image
│   └── claude/           Claude Code agent
├── schema/               JSON Schema for settings
└── scripts/              Install scripts
```

## Attribution

Based on concepts from [Hole](https://github.com/lukashornych/hole) by Lukas Hornych, licensed under Apache 2.0.

## License

Apache 2.0 — see [LICENSE](LICENSE).
