# Shared Memory

Shared persistent memory layer for two users. Stores notes with vector embeddings (Ollama / nomic-embed-text), enables semantic search, and exposes an MCP server for direct integration with Claude Code and claude.ai.

## Architecture

```
                    ┌────────────┐
                    │   nginx    │ :80 (prod only)
                    └──────┬─────┘
           ┌───────────────┼───────────────┐
           v               v               v
     ┌──────────┐   ┌──────────┐   ┌──────────┐
     │ frontend │   │   api    │   │   mcp    │
     │ :3000    │   │ :3001    │   │ :3002    │
     └──────────┘   └────┬─────┘   └────┬─────┘
                         │              │ (HTTP → api)
              ┌──────────┼──────────┐   │
              v          v          v   │
        ┌──────────┐ ┌────────┐ ┌────────┐
        │ postgres │ │ qdrant │ │ ollama │
        │ :5432    │ │ :6333  │ │ :11434 │
        └──────────┘ └────────┘ └────────┘
```

| Layer | Tech |
|-------|------|
| API | Hono + TypeScript |
| Vectors | Qdrant (cosine, 768d) |
| Metadata & relations | PostgreSQL 16 |
| Embeddings | Ollama + nomic-embed-text |
| MCP server | @modelcontextprotocol/sdk + Express |
| Frontend | Next.js 15 + Tailwind + react-force-graph |
| Orchestration | Docker Compose |

## Quick start (development)

### Prerequisites

- Node.js 20+
- pnpm 9+
- Docker & Docker Compose v2

### 1. Configure environment

```bash
cp .env.example .env
```

Edit `.env` — generate API keys:

```bash
# Generate 64-char hex keys
openssl rand -hex 32   # → USER_1_API_KEY
openssl rand -hex 32   # → USER_2_API_KEY
openssl rand -hex 32   # → MCP_API_KEY
```

Create `.env.dev` (dev overrides — services run on localhost):

```bash
cp .env.example .env.dev
```

In `.env.dev`, change the service URLs to point at localhost:

```env
DATABASE_URL=postgresql://memory:memory@localhost:5432/memory
QDRANT_URL=http://localhost:6333
OLLAMA_URL=http://localhost:11434
API_URL=http://localhost:3001
NEXT_PUBLIC_API_URL=http://localhost:3001
```

### 2. Install dependencies

```bash
pnpm install
```

### 3. Start infrastructure + all services

```bash
# Starts postgres, qdrant, ollama containers, then api + mcp + frontend concurrently
pnpm dev
```

### 4. Pull the embedding model (first time only)

```bash
pnpm docker:dev:pull-model
```

The API is now at `http://localhost:3001`, frontend at `http://localhost:3000`.

### Stopping

```bash
# Stop the dev containers (data is persisted in Docker volumes)
pnpm docker:dev:down
```

## Production deployment

Production uses a single `docker-compose.yaml` generated from `docker-compose.template.yaml` with env substitution. An nginx reverse proxy sits in front of all services — only nginx is exposed to the host network.

```bash
# 1. Configure .env.prod with real passwords and keys
# 2. Build and start
pnpm docker:prod:up

# 3. Pull embedding model (first time)
pnpm docker:prod:pull-model
```

CI/CD is configured via `.gitlab-ci.yml` — pushes to `master` build the Docker image and deploy over SSH.

## API reference

All endpoints require authentication via `X-API-Key` header.

### Health check

```
GET /health → { "status": "ok" }
```

### Notes CRUD

```
POST   /notes              Create a note
GET    /notes/:id          Get note with its links
PUT    /notes/:id          Update note (author only)
DELETE /notes/:id          Delete note (author only)
```

**POST /notes** body:
```json
{ "content": "Note text", "tags": ["optional", "tags"] }
```

**PUT /notes/:id** body (all fields optional):
```json
{ "content": "Updated text", "tags": ["new", "tags"] }
```

Changing `content` regenerates the embedding. Changing only `tags` updates the Qdrant payload without re-embedding.

### Note links

```
POST   /notes/:id/links      Create/upsert a link
DELETE /notes/:id/links/:lid  Delete a link
GET    /notes/:id/links       List links for a note
```

**POST /notes/:id/links** body:
```json
{ "targetId": "uuid", "relation": "caused by" }
```

`relation` is free text, max 100 chars. Duplicate links (same pair) update the relation.

### Semantic search

```
GET /context/retrieve?q=search+query
```

| Param | Type | Description |
|-------|------|-------------|
| `q` | string | **Required.** Search query |
| `author` | string | Filter by username |
| `tags` | string | Comma-separated tag filter |
| `limit` | number | Max results (default 10, max 50) |
| `include_linked` | string | `"true"` to include linked notes via graph traversal |

Returns notes sorted by similarity score, each with their links.

### Stats

```
GET /stats → { notes: { total, by_user }, links: { total }, qdrant: { ... } }
```

### Admin

```
POST /admin/reindex
```

Re-embeds all notes into Qdrant. Use after a lost Qdrant volume or embedding model change.

## MCP server

The MCP server exposes 4 tools over StreamableHTTP transport (stateless mode) on port 3002.

### Tools

| Tool | Description |
|------|-------------|
| `search_memory` | Semantic search with optional author/tags/limit filters and `include_linked` graph traversal |
| `add_note` | Create a new note with content and optional tags |
| `link_notes` | Link two notes with a relation description |
| `get_note` | Retrieve a note by ID with metadata and links |

### Claude Code integration

Add to your Claude Code MCP config:

```json
{
  "mcpServers": {
    "shared-memory": {
      "type": "streamable-http",
      "url": "https://your-domain.com/mcp/"
    }
  }
}
```

The MCP server includes a minimal OAuth 2.0 implementation (no real auth — security is at the API key level) required by the Claude Code MCP SDK.

## Project structure

```
shared-memory/
├── packages/
│   ├── api/           # Hono REST API
│   ├── mcp/           # MCP server (StreamableHTTP)
│   └── frontend/      # Next.js web app
├── infra/
│   ├── postgres/      # init.sql schema
│   └── nginx/         # Reverse proxy config
├── docker-compose.dev.yaml       # Dev infrastructure
├── docker-compose.template.yaml  # Prod template (envsubst)
├── docker-compose.prod.yaml      # Prod overrides
└── .gitlab-ci.yml                # CI/CD pipeline
```

## Database schema

Three tables: `users`, `notes`, `note_links`. See `infra/postgres/init.sql` for the full schema.

- Notes have `content`, `tags` (text array), and a `qdrant_id` linking to the vector store
- Links are bidirectional with a free-text `relation` field
- GIN index on tags, B-tree indexes on foreign keys

## Scripts reference

| Script | Description |
|--------|-------------|
| `pnpm dev` | Start dev infra + all services |
| `pnpm dev:api` | API only |
| `pnpm dev:mcp` | MCP server only |
| `pnpm dev:frontend` | Frontend only |
| `pnpm build:all` | Build all packages |
| `pnpm docker:dev:up` | Start dev containers |
| `pnpm docker:dev:down` | Stop dev containers |
| `pnpm docker:dev:pull-model` | Pull nomic-embed-text into dev Ollama |
| `pnpm docker:prod:up` | Build & start production stack |
| `pnpm docker:prod:down` | Stop production stack |
| `pnpm docker:prod:pull-model` | Pull model into prod Ollama |
