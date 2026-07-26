---
name: sandseal
description: How to change the configuration of the Sandseal sandbox you are running inside — settings.json layers and merge order, profiles, $replace, hiding and exposing files, extra host directories, reaching services on the host, system packages, hooks, env vars, and which command applies each change. Use whenever the sandbox blocks or lacks something: a missing tool or package, a file you cannot read (often a secret), a database or port you cannot reach, a directory outside the project, an environment variable, an out-of-memory build, or when the user says "add it to the sandbox config", "why can't I see X", "let me reach Y", "settings.json", "sandseal profile".
---

# Configuring the sandbox you run in

You are an agent inside a Sandseal sandbox: a Docker container with this project mounted at
its real host path. The isolation you notice — files that are empty, ports that do not
answer, directories that do not exist — is deliberate and configurable.

## What you can and cannot do

**You can** edit `.sandseal/settings.json` in the project. It is inside the mounted project
directory, so writes land on the host and persist.

**You cannot:**

- **Apply the change.** There is no `sandseal` binary in the container. Configuration takes
  effect when the sandbox is started again, which only the user can do from the host.
- **Change the machine-wide config.** `~/.sandseal/settings.json` *is* mounted here, but
  read-only, so you can read it to explain your own environment. Profiles
  (`~/.sandseal/profiles/`) and the active-profile state are **not** visible at all. When the
  behaviour you see is not explained by the project file plus the global one, an active
  profile is the likely reason — say so and ask the user to run `sandseal config effective`
  rather than guessing.
- **Read excluded files.** A file that is empty or missing may be excluded on purpose (see
  below). Do not "fix" that by removing the exclusion.

So the shape of every answer is: **write the config, then tell the user exactly which
command to run.** Never claim a change is active.

## Where configuration comes from

Three layers, deep-merged bottom-up:

```
~/.sandseal/settings.json          machine-wide default   (mounted here read-only)
        ↓
<project>/.sandseal/settings.json  this project           (you can edit this)
        ↓
~/.sandseal/profiles/<name>.json   active profile, if any (wins; invisible to you)
```

Merge rules: objects merge recursively, **arrays concatenate and de-duplicate**, scalars from
the higher layer win. So a profile's `files.exclude` is *added* to the project's, while
`network.mode` replaces it.

**The profile is a ceiling, not a suggestion.** It sits on top precisely so a project cannot
re-open what a profile closed (`docker.passthrough`, `network.mode`). If a setting you wrote
appears to have no effect, an active profile is the first thing to suspect — and the right
response is to tell the user, not to work around it.

## Removing an inherited value: `$replace`

Merging can only add or overwrite a key, never remove one — `"environment": {}` leaves
inherited variables in place. To replace a subtree wholesale, list its path:

```json
{
  "$replace": ["environment", "files.include"],
  "environment": { "ONLY_THIS": "1" }
}
```

Listed paths are deleted from the accumulator *before* the merge, so this layer's value wins
whole. Rules that matter:

- **One path per array item.** `["environment, files"]` is a single unknown path, not two.
- An unknown path is a **hard error**, not a warning — a typo that silently disabled the
  directive would leave inherited secrets exposed.
- Paths are dotted schema keys. A node with free-form keys (like `environment`) swallows the
  rest of the path, so `environment.API_TOKEN` works; keys containing dots cannot be
  addressed individually — replace the whole map.

## Settings reference

Everything below is the complete surface. The file is validated against a JSON schema and an
unknown key is an error, so do not invent fields.

### `files.exclude` — hide paths from the agent

```json
{ "files": { "exclude": [".env", ".env.*", "secrets/**", "*.pem"] } }
```

Excluded paths are masked inside the container (an empty file or missing directory). This is
the mechanism that keeps credentials away from an agent. If you need a *value* from a secret
file, ask the user for it — do not remove the exclusion to read it.

### `files.include` — mount extra host paths in

```json
{ "files": { "include": { "~/.aws/credentials": "~/.aws/credentials", "/etc/hosts": "/etc/hosts" } } }
```

Maps host path → path inside the sandbox. Use for a config or credential the task genuinely
needs. Prefer the narrowest path (one file, not the whole home directory).

### `workspace` — see another project

```json
{ "workspace": { "dir": "~/development", "readwrite": false } }
```

Mounts one host directory so you can read across projects. Read-only unless
`readwrite: true`. Ask before requesting write access to code outside this project.

### `dependencies` — system packages in the image

```json
{ "dependencies": ["postgresql-client", "imagemagick", "ripgrep"] }
```

Installed when the image is built, so this **needs a rebuild** (below). For a one-off tool
you may simply `sudo apt-get install` it in the running container — but it is lost when the
sandbox is recreated, so put anything the project needs here.

### `network` — reaching things outside

Default is `bridge`: you have internet access, but **not** the host's `localhost`. So a
database running on the host is unreachable by default.

```json
{ "network": { "services": { "db.local": "host-gateway:5432" } } }
```

`services` maps a hostname you can then use (`db.local:5432`) to a target, and generates
`extra_hosts`. **Prefer this over host mode**: it opens exactly one service.

```json
{ "network": { "mode": "host" } }
```

Host mode shares the host's whole network namespace — every local service, the local
network, and on a cloud host the metadata endpoint. Only suggest it when a task genuinely
cannot work otherwise, and say plainly what it opens.

### `environment` — variables in the container

```json
{ "environment": { "NODE_ENV": "development", "RUST_LOG": "debug" } }
```

Do not put secrets here: the file is usually committed. Use `files.include` for a credential
file, or have the user pass it another way.

### `container` — resources and base image

```json
{ "container": { "memoryLimit": "8g", "memorySwapLimit": "8g", "baseImage": "ubuntu:24.04" } }
```

Reach for `memoryLimit` when a build dies with an out-of-memory or a bare "killed".
`baseImage` needs a rebuild.

### `hooks` — scripts around the lifecycle

```json
{
  "hooks": {
    "setup": { "script": "pnpm install --frozen-lockfile" },
    "prestart": [{ "script": "/workspace/scripts/wait-for-db.sh" }],
    "setupHost": [{ "script": "docker compose -f dev.yaml up -d" }],
    "cleanupHost": [{ "script": "docker compose -f dev.yaml down" }]
  }
}
```

| Hook | Runs | Where |
|---|---|---|
| `setup` | at image build | in the image (needs a rebuild) |
| `prestart` | before the agent starts, every start | in the container |
| `setupHost` | before the container starts | **on the host** |
| `cleanupHost` | after the container stops | **on the host** |

`setupHost` and `cleanupHost` execute on the user's machine outside the sandbox. Treat them
as a request for host access: propose, explain, and let the user decide.

### `docker.passthrough` — Docker socket

```json
{ "docker": { "passthrough": true } }
```

Gives the container the host's Docker socket, which is effectively root on the host. Do not
enable it to work around a smaller problem. A profile may forbid it outright.

## Which command applies the change

| Changed | Needed |
|---|---|
| `dependencies`, `hooks.setup`, `container.baseImage` | `sandseal start --rebuild` (or `sandseal build` then `sandseal start`) |
| everything else | next `sandseal start` |

Useful commands to hand the user, all run **on the host**:

| Command | Purpose |
|---|---|
| `sandseal config effective` | the merged result of all layers — the answer to "why is this happening" |
| `sandseal config list` | profiles and which one is active |
| `sandseal config show [name]` | one profile's contents |
| `sandseal config set <name>` / `create` / `edit` / `delete` | manage profiles |
| `sandseal start --profile <name>` / `--no-profile` | override the active profile for one run |
| `sandseal build [path]` | rebuild the image without starting |
| `sandseal destroy` | remove this project's sandbox |

Failures are loud on purpose: a missing profile aborts the start rather than silently
dropping its restrictions, and an unknown `$replace` path is an error.

## How to answer a configuration request

1. Name the cause. "The host Postgres is unreachable because the sandbox uses bridge
   networking" beats "let me add a setting".
2. Write the smallest change into `.sandseal/settings.json` that fixes it — one service, not
   host mode; one file, not the home directory.
3. Say whether a rebuild is needed, and give the exact command.
4. If a security default is in the way (`files.exclude`, `network.mode`,
   `docker.passthrough`), explain the trade-off and let the user choose. Do not quietly
   widen access, and do not remove an exclusion in order to read a secret.

### Worked examples

**"The app can't reach my local database."**

```json
{ "network": { "services": { "db.local": "host-gateway:5432" } } }
```

Then: connect to `db.local:5432`, and `sandseal start` to apply. No rebuild.

**"`psql` isn't installed."**

```json
{ "dependencies": ["postgresql-client"] }
```

Then: `sandseal start --rebuild`. For right now, `sudo apt-get install -y postgresql-client`
works but is lost when the sandbox is recreated.

**"`.env` is empty."**

That is `files.exclude` working as intended — the sandbox hides secrets from the agent. Ask
for the specific value you need, or have the user mount a narrower file via `files.include`.
Do not remove the exclusion.

**"Builds keep getting killed."**

```json
{ "container": { "memoryLimit": "8g", "memorySwapLimit": "8g" } }
```

Then: `sandseal start`. No rebuild — but check the host has the RAM.
