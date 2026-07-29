# What Sandseal touches

A complete account of every file, volume and container Sandseal reads, writes or mounts, and
what is in each one. If you want to know what running `sandseal start` does to your machine
before you run it, this is the page.

The short version: Sandseal keeps its own state under `~/.sandseal/` and `~/.config/sandseal/`,
creates Docker resources prefixed `sandseal-sandbox`, and configures the agent inside the
container through command-line flags pointing at read-only mounts. It installs nothing
globally and does not modify your agent's own configuration files.

Throughout this page, "Sandseal writes" means the CLI itself. The **agent** running inside the
sandbox writes to your project — that is the point of the tool, and those are ordinary file
changes you review with `git diff`.

---

## On the host

### `~/.sandseal/`

| Path | Written by | Contents |
|---|---|---|
| `settings.json` | you | Machine-wide defaults: dependencies, file exclusions, workspace, network, hooks, environment. Read on every start. Sandseal never writes to it. |
| `profiles/<name>.json` | `sandseal config` | Named settings presets you can switch between per project. |
| `state.json` | `sandseal config use` | Which profile is active machine-wide. Machine-local. |
| `tmp/<random>/` | `sandseal start` | Per-instance scratch: the generated compose override, rendered memory config, prestart scripts, and placeholder directories used to mask excluded paths. One directory per sandbox run. |
| `instances/<instance>.json` | `sandseal start` | One record per running sandbox — which containers, tmp dir and session belong to it. The CLI holds an exclusive `flock` on it while the sandbox runs, so a record whose lock is free identifies a sandbox whose CLI is gone. Deleted on a clean exit, collected by `sandseal gc` otherwise. |
| `keys/identity.key`, `keys/identity.pub` | `sandseal pair` / `connect` | Keypair identifying this machine to the dashboard for end-to-end encrypted remote sessions. Created on first use of a remote feature, not at install time. |

Only `settings.json` is created up front. The rest appear when you first use the feature that
needs them, so a fresh install that only ever runs `sandseal start` leaves a directory with a
single file in it.

### `~/.config/sandseal/auth.json`

Your account token, written by `sandseal login`. Deliberately **not** under `~/.sandseal/` — it
follows the platform convention (`dirs::config_dir()`), so on macOS it is
`~/Library/Application Support/sandseal/auth.json` and on Linux it honours `XDG_CONFIG_HOME`.

**This file never enters the container.** The sandbox receives a per-session credential instead,
which is revoked when the session ends.

### In the project directory

| Path | Written by | Contents |
|---|---|---|
| `.sandseal/settings.json` | you | Project settings, merged over the machine-wide ones. Arrays concatenate and de-duplicate; `$replace` overrides a path wholesale. |
| `.sandseal/state.json` | `sandseal config use` | Active profile for this project. Machine-local — add it to `.gitignore`. |
| `.sandseal/.runtime-packages` | the sandbox | Log of packages installed at runtime inside the container, so Sandseal can suggest moving them into `dependencies` after you exit. |

Nothing else in your project is created or modified. The project directory is mounted
read-write, so anything the agent writes is a normal file change you can inspect with `git diff`.

---

## What Sandseal does *not* change

Worth stating explicitly, because an earlier version did and it was a mistake:

- **`~/.claude.json` and `~/.claude/settings.json` are never written.** Agent configuration is
  passed on the command line (`--mcp-config`, `--settings`) pointing at read-only mounts. Two
  reasons: people bind-mount those paths into the container to carry a login, and you cannot
  rename over a bind mount — the write fails with `EBUSY`. Worse, where the write *succeeds* it
  lands on the host's real files, so a sandbox would install its hooks into every agent session
  on the machine.
- **No global packages are installed**, on the host or otherwise. System packages listed in
  `dependencies` are baked into the sandbox image.
- **`~/.sandseal/settings.json` is mounted read-only into the container.** The agent can read
  the machine defaults to explain its own environment, but cannot rewrite them — a writable
  mount would let a sandboxed agent drop `files.exclude` or enable `docker.passthrough` for
  every future session on the machine.

### One exception to know about

The container entrypoint copies Sandseal's bundled skills into `$HOME/.claude/skills/` **inside
the container**, refreshed on every start so a CLI upgrade ships updated skills automatically.

If you mount your host `~/.claude` into the sandbox (via `files.include`), that copy lands on
your **host** filesystem. You will see a `sandseal/` directory appear in `~/.claude/skills/`.
That is expected, and it is the only thing Sandseal writes outside its own directories.

---

## Docker resources

| Resource | Name | Notes |
|---|---|---|
| Base image | `sandseal-sandbox/agent-<agent>:base-<hash>` | Project-agnostic and **shared** by every project with the same base image, user and agent installs — rebuilding it updates all of them at once. |
| Overlay image | `sandseal-sandbox/agent-<agent>:<project>-<hash>` | Built **only** if the project declares `dependencies` or a setup hook. Otherwise the sandbox runs the base image directly. |
| Container | `sandseal-sandbox-<project>-<hash>-<instance>-agent-1` | One per running sandbox. |
| Volume | `…_sandseal-agent-home` | Agent home. Persists CLI logins, installed user-level tools, `~/.cargo`, `~/.local`. Survives container restarts. |
| Volume | `…_sandseal-apt-cache` | Shared apt cache, so repeated installs are fast. |
| Labels | `sandseal.project_name`, `sandseal.project_dir`, `sandseal.instance_name` | How `sandseal status`, `sandseal destroy` and `sandseal gc` find instances. |

`sandseal destroy` removes a project's sandboxes; `sandseal destroy --all` removes every one on
the machine. Volumes persist by design — that is what makes an agent's login and installed
tooling survive a restart.

**Containers do not outlive the CLI.** A sandbox is torn down when you exit it, and also when
the CLI is signalled — closing the terminal sends SIGHUP, which stops the container instead of
killing the CLI and leaving it running. What that cannot cover is `kill -9` or a machine that
loses power, so `sandseal start` also sweeps first: any sandbox whose instance record is
unlocked has no CLI behind it and is taken down, along with containers from sessions that
already ended. `sandseal gc` runs the same sweep on demand, `sandseal gc --dry-run` only
reports it, and `sandseal status` says how many abandoned sandboxes it can see.

The sweep never touches a sandbox someone is using: a running CLI holds its record's lock, and
a locked record is skipped. Nor does it touch anything without the `sandseal.project_name`
label, so the rest of what runs on your machine is outside its reach. Containers started by a
Sandseal older than instance records have no record at all — those are left alone while
running, and removed once they stop.

The sweep on `sandseal start` is opt-out. `{"gc": {"onStart": false}}` in settings leaves it to
`sandseal gc`, which is worth setting only if you deliberately keep sandboxes running with no
CLI attached — detaching one properly (tmux, `nohup`) keeps its process, and therefore its
lock, alive, so it survives the sweep either way.

**System packages do not survive an image rebuild.** Anything you `apt install` inside a running
sandbox applies to that instance only; put it in `dependencies` to make it permanent.

---

## Mounts inside the container

| Container path | Mode | What it is |
|---|---|---|
| *the project path, unchanged* | rw | Your project, at the same absolute path as on the host, so paths in output are copy-pasteable. |
| paths from `files.exclude` | ro / rw | Masked, not removed. A **file** gets `/dev/null` mounted over it read-only; a **directory** gets an empty directory from the instance's `tmp/excluded-dirs/`. Either way the path still exists and reads as empty — which is why an empty `.env` inside a sandbox usually means "excluded", not "missing". |
| paths from `files.include` | rw/ro | Extra host paths you chose to expose. |
| `workspace.dir` | rw/ro | An additional directory tree, for working across sibling repositories. |
| `~/.sandseal/settings.json` | **ro** | Machine defaults, readable but not writable. |
| `/opt/sandseal/skills` | ro | Bundled skills, copied into the agent home at startup. |
| `/usr/local/bin/sandseal` | ro | The Sandseal binary itself, mounted so the in-container memory bridge can run. Only when memory is on. |
| `/var/run/docker.sock` | rw | **Only if `docker.passthrough` is enabled.** Full Docker access — that is host-level privilege, so turn it on deliberately. |
| `/tmp/prestart-scripts` | ro | Your prestart hook scripts. |
| `/run/sandseal/mcp.json` | ro | Memory MCP server registration. Only when memory is on. |
| `/run/sandseal/settings.json` | ro | Memory recall hook and the built-in-memory switch. Only when memory is on. |

---

## Memory configuration

Only rendered when memory is active — that is, when you are logged in, your subscription covers
it, and the backend is reachable. Every failure path means "this sandbox has no memory", never
a failed start.

Two files are generated on the **host**, into the instance's `tmp/memory/` directory, then
mounted read-only. They are passed to the agent as `--mcp-config` and `--settings`. Neither
flag replaces your own configuration: both merge with it, and Sandseal deliberately does not
pass `--strict-mcp-config`, so your own MCP servers keep working.

**`/run/sandseal/mcp.json`**

```json
{
  "mcpServers": {
    "sandseal-memory": { "type": "stdio", "command": "sandseal", "args": ["memory", "mcp"] }
  }
}
```

A local stdio server, not a remote endpoint. No URL and no token appear in any config file, so
a prompt injection has nothing to point somewhere else.

**`/run/sandseal/settings.json`**

```json
{
  "autoMemoryEnabled": false,
  "hooks": {
    "UserPromptSubmit": [
      { "hooks": [{ "type": "command", "command": "sandseal memory recall --stdin" }] }
    ]
  }
}
```

The hook injects relevant notes before each prompt. It fails open and silent: if the backend is
slow or down, you get no notes rather than an error.

`autoMemoryEnabled: false` switches off Claude Code's own file-based memory, which is on by
default and would otherwise run alongside Sandseal's — a second set of memory instructions in
the system prompt, telling the agent to write notes into a directory inside the agent home
volume that nothing reads back. It is set here, in a file rendered only when Sandseal's memory
is active, so a sandbox *without* a subscription keeps the built-in memory rather than ending
up with none.

### Environment variables

Set in the container only when memory is on:

| Variable | Contents |
|---|---|
| `SANDSEAL_MEMORY_TOKEN` | Per-session credential. Revoked the moment the session ends. |
| `SANDSEAL_API_URL` | Backend the credential belongs to. |
| `SANDSEAL_MEMORY_CROSS_PROJECT` | `1` or `0` — whether recall spans your whole space or only this project. Always written, never omitted. |

Always set:

| Variable | Contents |
|---|---|
| `SANDSEAL_RUNTIME_PACKAGES` | Path to the runtime package log described above. |

Plus anything you list under `environment` in your settings, with `${VAR}` references expanded
from the host environment.

---

## Removing everything

```bash
sandseal destroy --all                           # every sandbox on this machine
docker volume ls -q --filter name=sandseal-sandbox | xargs -r docker volume rm
docker images 'sandseal-sandbox/*' -q | xargs -r docker rmi
rm -rf ~/.sandseal ~/.config/sandseal            # settings, profiles, keys, token
```

Then remove the `.sandseal/` directory from any project you used it in. The uninstall script
(`scripts/uninstall.sh`) removes the binary itself.
