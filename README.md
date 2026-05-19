# Sandseal

Isolated Docker sandboxes for AI coding agents.

Run Claude Code (and other AI agents) in a secure, containerized environment with fine-grained file access control, custom dependencies, and host networking — without touching your host system.

## Quick start

```bash
curl -fsSL https://sandseal.io/install.sh | bash
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
      "/home/me/.ssh/config": "/home/agent/.ssh/config"
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

## CLI usage

```
sandseal start [path]      Start a sandbox (default: current directory)
sandseal start -d [path]   Start in debug mode (bash shell)
sandseal start --rebuild   Force rebuild the Docker image
sandseal destroy [path]    Destroy sandbox for a project
sandseal destroy --all     Destroy all sandboxes
sandseal status            Show running sandboxes
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
