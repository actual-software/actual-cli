# 04 - API Contract (ADR Bank)

## Overview

The ADR Bank API is a REST service that stores and serves Architecture Decision Records. The `actual` CLI queries it to find ADRs matching the detected languages and frameworks. The backend is developed separately -- this document defines the contract the CLI expects.

## Base URL

Default: `https://api-service.api.prod.actual.ai` (configurable via `--api-url` flag or config file)

This is the sprintreview api-service. All endpoints are at the root level (no `/v1/` prefix), consistent with existing sprintreview routes. New endpoints defined below will be implemented by the backend team.

## Authentication

**ADR endpoints (v1)**: Unauthenticated. No API key required.

**Telemetry endpoint**: Authenticated via embedded service key (`Authorization: ServiceKey <key>`). See [08-auth-strategy.md](./08-auth-strategy.md).

**Future**: ADR endpoints may require an API key or OAuth token.

## Endpoints

### `POST /adrs/match`

Find ADRs matching a set of languages and frameworks.

#### Request

```json
{
  "projects": [
    {
      "path": "apps/web",
      "name": "Web Frontend",
      "languages": ["typescript"],
      "frameworks": [
        { "name": "nextjs", "category": "web-frontend" },
        { "name": "react", "category": "web-frontend" }
      ]
    },
    {
      "path": "apps/api",
      "name": "API Service",
      "languages": ["typescript"],
      "frameworks": [
        { "name": "fastify", "category": "web-backend" }
      ]
    }
  ],
  "options": {
    "include_general": true,
    "categories": null,
    "exclude_categories": null,
    "max_per_framework": null
  }
}
```

**Fields:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `projects` | `Project[]` | Yes | Detected projects from repo analysis |
| `projects[].path` | `string` | Yes | Relative path (`.` for root) |
| `projects[].name` | `string` | Yes | Human-readable name |
| `projects[].languages` | `string[]` | Yes | Language identifiers |
| `projects[].frameworks` | `Framework[]` | Yes | Detected frameworks |
| `options.include_general` | `bool` | No | Include language-agnostic ADRs (default: true) |
| `options.categories` | `string[]` | No | Only return ADRs in these category paths |
| `options.exclude_categories` | `string[]` | No | Exclude ADRs in these category paths |
| `options.max_per_framework` | `int` | No | Limit ADRs per framework (no limit by default) |

#### Response

```json
{
  "matched_adrs": [
    {
      "id": "550e8400-e29b-41d4-a716-446655440000",
      "title": "Use App Router for all new pages",
      "context": "Next.js 13+ introduced the App Router as the recommended routing paradigm. The Pages Router is maintained but not the direction Next.js is heading.",
      "policies": [
        "Use the App Router (app/ directory) for all new pages and routes",
        "Do not create new files in the pages/ directory",
        "Migrate existing Pages Router routes to App Router when modifying them"
      ],
      "instructions": [
        "New routes go in app/ using the file-based routing convention (page.tsx, layout.tsx, etc.)",
        "Use generateStaticParams for static generation instead of getStaticPaths"
      ],
      "category": {
        "id": "cat-123",
        "name": "Rendering Model",
        "path": "UI/UX & Frontend Decisions > Rendering Model"
      },
      "applies_to": {
        "languages": ["typescript", "javascript"],
        "frameworks": ["nextjs"]
      },
      "matched_projects": ["apps/web"]
    }
  ],
  "metadata": {
    "total_matched": 32,
    "by_framework": {
      "nextjs": 12,
      "react": 8,
      "fastify": 5,
      "typescript": 15,
      "general": 6
    },
    "deduplicated_count": 32
  }
}
```

**ADR Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `id` | `string` | UUID of the ADR |
| `title` | `string` | Short descriptive title |
| `context` | `string` | Background/provenance explaining why this ADR exists |
| `policies` | `string[]` | Actionable rules (the core of the ADR) |
| `instructions` | `string[]` | Optional how-to guidance |
| `category` | `Category` | Hierarchical category from the taxonomy |
| `applies_to.languages` | `string[]` | Languages this ADR is relevant to |
| `applies_to.frameworks` | `string[]` | Frameworks this ADR is relevant to |
| `matched_projects` | `string[]` | Which of the request's projects this ADR matched against |

### `GET /taxonomy/languages`

List all supported language identifiers.

```json
{
  "languages": [
    { "id": "typescript", "display_name": "TypeScript", "aliases": ["ts"] },
    { "id": "python", "display_name": "Python", "aliases": ["py", "python3"] }
  ]
}
```

### `GET /taxonomy/frameworks`

List all supported framework identifiers.

```json
{
  "frameworks": [
    { "id": "nextjs", "display_name": "Next.js", "category": "web-frontend", "languages": ["typescript", "javascript"] },
    { "id": "fastapi", "display_name": "FastAPI", "category": "web-backend", "languages": ["python"] }
  ]
}
```

### `GET /taxonomy/categories`

List the ADR category taxonomy (hierarchical).

```json
{
  "categories": [
    {
      "id": "cat-001",
      "name": "Language & Paradigm",
      "path": "Language & Paradigm",
      "level": 1,
      "children": [
        {
          "id": "cat-002",
          "name": "Programming Languages",
          "path": "Language & Paradigm > Programming Languages",
          "level": 2,
          "children": []
        }
      ]
    }
  ]
}
```

### `GET /health`

Health check endpoint.

```json
{
  "status": "ok",
  "version": "1.0.0"
}
```

## Error Responses

All errors follow a consistent format:

```json
{
  "error": {
    "code": "INVALID_REQUEST",
    "message": "At least one project is required",
    "details": null
  }
}
```

**Error Codes:**

| HTTP Status | Code | Description |
|-------------|------|-------------|
| 400 | `INVALID_REQUEST` | Malformed or invalid request body |
| 404 | `NOT_FOUND` | Endpoint not found |
| 422 | `UNKNOWN_TAXONOMY` | Unknown language or framework identifier |
| 429 | `RATE_LIMITED` | Too many requests |
| 500 | `INTERNAL_ERROR` | Server error |
| 503 | `UNAVAILABLE` | Service temporarily unavailable |

## CLI Behavior

1. **Request construction**: After repo analysis and user confirmation, build the `POST /adrs/match` request body from detected projects
2. **Taxonomy validation**: Optionally fetch `/taxonomy/languages` and `/taxonomy/frameworks` to validate detected identifiers before the match request (cache locally)
3. **Error handling**: On 4xx, display the error message; on 5xx/timeout, retry up to 3 times with exponential backoff
4. **Offline fallback**: If API is unreachable after retries, inform user and exit (no local ADR fallback in v1)

### `POST /counter/record`

Telemetry endpoint (already exists in sprintreview api-service). Used to report sync metrics.

#### Request

```json
{
  "metrics": [
    {
      "name": "cli.sync.adrs_fetched",
      "value": 32,
      "tags": {
        "repo_hash": "sha256(origin_url + HEAD)",
        "source": "actual-cli",
        "version": "0.1.0"
      }
    },
    {
      "name": "cli.sync.adrs_tailored",
      "value": 28,
      "tags": {
        "repo_hash": "sha256(origin_url + HEAD)",
        "source": "actual-cli"
      }
    },
    {
      "name": "cli.sync.adrs_rejected",
      "value": 3,
      "tags": {
        "repo_hash": "sha256(origin_url + HEAD)",
        "source": "actual-cli"
      }
    },
    {
      "name": "cli.sync.adrs_written",
      "value": 25,
      "tags": {
        "repo_hash": "sha256(origin_url + HEAD)",
        "source": "actual-cli"
      }
    }
  ]
}
```

**Authentication**: `Authorization: ServiceKey <embedded_service_key>`

**Opt-out**: Users can disable telemetry via `actual config set telemetry.enabled false` or by setting the `ACTUAL_NO_TELEMETRY=1` environment variable.

**Metric names**:

| Metric | Description |
|--------|-------------|
| `cli.sync.adrs_fetched` | Count of ADRs returned from the API |
| `cli.sync.adrs_tailored` | Count of ADRs after tailoring (applicable only) |
| `cli.sync.adrs_rejected` | Count of ADRs rejected by the user |
| `cli.sync.adrs_written` | Count of ADRs written to CLAUDE.md |

**Tags**: All metrics include `repo_hash`, `source` (`actual-cli`), and `version` (CLI version). No source code or repo URLs are transmitted.

**`repo_hash` generation**:
- If git remote exists: SHA-256 of `git remote get-url origin` + HEAD commit
- If git repo but no remote: SHA-256 of absolute directory path + HEAD commit
- If not a git repo: SHA-256 of absolute directory path

## Deduplication

The CLI trusts the server for deduplication. The `matched_projects` field on each ADR lists all project paths the ADR applies to. Universal ADRs (e.g., "use conventional commits") list every project path from the request -- there is no sentinel value.

## Rate Limiting

The CLI should:
- Respect `Retry-After` headers on 429 responses
- Implement exponential backoff (1s, 2s, 4s) for retries
- Cache taxonomy responses locally (refresh daily)
