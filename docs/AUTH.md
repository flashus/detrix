# Authentication & Authorization

Detrix supports multi-tenant access control with per-user identity, role-based authorization, and per-agent metric isolation.

---

## Auth Modes

| Mode | Config | Description |
|------|--------|-------------|
| **Auto** | `[api.auth]` section absent | Secure by default. Daemon auto-generates a token saved to `~/detrix/auth-token`. MCP bridge discovers it automatically. |
| **Disabled** | `mode = "disabled"` | No authentication. All requests get Admin access. Use only for local development. |
| **Simple** | `mode = "simple"` | Per-user static bearer tokens defined in `detrix.toml`. Each token maps to a user identity and role. |
| **External** | `mode = "external"` | JWT validation via a JWKS endpoint. For enterprise SSO (Keycloak, Auth0, etc.). |

### Auto-Auth (Default)

When the `[api.auth]` section is omitted entirely, the daemon automatically:

1. Generates a cryptographically secure 64-character hex token (256 bits of entropy)
2. Saves it to `~/detrix/auth-token` (permissions `0600`)
3. Enables Simple mode with the generated token as a single Admin user

The MCP bridge (`detrix mcp`) discovers this token automatically via the `~/detrix/auth-token` file. No configuration needed for single-user local development.

You can override the auto-generated token with the `DETRIX_TOKEN` environment variable:

```bash
# Use a specific token instead of auto-generated
DETRIX_TOKEN=my-custom-token detrix serve
```

---

## Simple Mode

Define static tokens in `detrix.toml`. Each user gets a unique bearer token, a user ID (stamped on every metric they create), and a role.

```toml
[api.auth]
mode = "simple"

[[api.auth.users]]
token = "dtx_alice_3f9a..."
user_id = "alice"
role = "user"

[[api.auth.users]]
token = "dtx_bob_8e2c..."
user_id = "bob"
role = "user"

[[api.auth.users]]
token = "dtx_admin_7c2b..."
user_id = "admin"
role = "admin"
```

**How to use:** Pass the token as a Bearer header:

```bash
# REST
curl -H "Authorization: Bearer dtx_alice_3f9a..." http://localhost:8090/api/v1/metrics

# MCP bridge
DETRIX_TOKEN=dtx_alice_3f9a... detrix mcp
```

**Roles:**
- `user` — can create, read, and manage their own metrics
- `admin` — full access to all metrics across all users

### Token Requirements

- Minimum length: 16 characters
- Maximum length: 512 characters
- Tokens must be unique across all users
- User IDs must be unique across all users
- Tokens are compared using constant-time SHA-256 comparison to prevent timing side-channels

### Tenant ID Validation

The `user_id` field (and `agent_id`) are validated at API boundaries:

- Must not be empty
- Must not be whitespace-only (e.g., `"   "`, `"\t"`)
- Must not contain control characters (e.g., null bytes, newlines)
- Must not use the reserved `__*__` pattern (e.g., `__system__`, `__admin__`)
- Maximum length: 256 characters

Invalid tenant IDs return HTTP 400 / gRPC `INVALID_ARGUMENT` with error code `1008` (`INVALID_TENANT_ID`).

---

## External Mode (JWT/JWKS)

For enterprise deployments with an identity provider (Keycloak, Auth0, Okta, etc.).

```toml
[api.auth]
mode = "external"

[api.auth.jwt]
jwks_url = "https://auth.example.com/.well-known/jwks.json"
issuer = "https://auth.example.com"
audience = "detrix"
cache_ttl_seconds = 300

# Optional: map a JWT claim to admin role
admin_role_claim = "roles"          # claim name (e.g. "roles", "realm_access.roles")
admin_role_value = "detrix-admin"   # value that grants admin access
```

The JWT `sub` claim becomes the `user_id` on metrics. JWTs without a `sub` claim are rejected with HTTP 401. JWKS keys are cached for `cache_ttl_seconds` (default: 300s / 5 minutes).

**Admin role mapping:** If `admin_role_claim` and `admin_role_value` are both set, the daemon checks the specified claim in the JWT. Nested claims are supported with dot notation (e.g., `realm_access.roles`). If the claim contains the configured value, the user gets Admin access. Otherwise, they get User access. Both fields must be set together or both omitted.

---

## Identity Model

Every metric carries two identity fields:

| Field | Source | Purpose |
|-------|--------|---------|
| `user_id` | Authenticated user (from token or JWT `sub`) | Owns the metric. Used for access control. |
| `agent_id` | MCP client name or `X-Detrix-Client-Id` header | Tracks which agent/session created the metric. |

Both are `Option<String>` — `None` for metrics created before auth was enabled or when auth is disabled. Metrics with no `user_id` are treated as system metrics internally (stored with the `__system__` sentinel in the database, mapped back to `None` on read).

### How identity is stamped

- **REST API:** `user_id` from the bearer token; `agent_id` from the `X-Detrix-Client-Id` header
- **gRPC:** Same — extracted from request metadata
- **MCP:** `user_id` from `DETRIX_TOKEN` resolution; `agent_id` from the MCP client name (auto-assigned UUID)

---

## Access Control (MetricScope)

Access is enforced in the service layer via `MetricScope` — a three-variant enum derived from the authenticated user and agent identity.

```
MetricScope::Admin                          — admin role, full access
MetricScope::User("alice")                  — user-level access
MetricScope::Agent { user_id: "alice", agent_id: "uuid-1234" }  — agent-level access
```

### Access Matrix

| Operation | Own metrics | Other agent (same user) | Other user | Admin |
|-----------|-------------|-------------------------|------------|-------|
| **List / Query** | Yes | Yes (read-only) | No | Yes |
| **Get events** | Yes | Yes (read-only) | No | Yes |
| **Create** | Yes | — | — | Yes |
| **Update / Delete** | Yes | No (403) | No (403) | Yes |
| **Enable / Disable** | Yes | No (403) | No (403) | Yes |
| **Group enable/disable** | Own only | Skipped | Skipped | All |

**Key rules:**
- Agents within the same user can **see** each other's metrics but **cannot modify** them
- Admin bypasses all access checks
- Unauthorized mutations return HTTP 403 / gRPC `PERMISSION_DENIED`
- Listing endpoints push `user_id` filtering to the database level (not in-memory filtering)

### Scope Enforcement by Protocol

All metric-touching endpoints are scope-enforced across all protocols:

| Protocol | List/Query | Get | Create | Mutate | Stream |
|----------|-----------|-----|--------|--------|--------|
| **REST** | `MetricFilter.user_id` | `scope.can_read()` | `user_id` stamped | `scope` checked | `allowed_ids` at connect |
| **gRPC** | `MetricFilter.user_id` | `scope.can_read()` | `user_id` stamped | `scope` checked | `allowed_ids` filter |
| **MCP** | `MetricFilter.user_id` | `scope.can_read()` | `user_id` stamped | `scope` checked | N/A |
| **WebSocket** | N/A | N/A | N/A | N/A | `allowed_ids` at connect |

**Known limitation:** WebSocket `allowed_ids` are computed once at connection time. Metrics created after the WebSocket upgrade are invisible to non-admin users. Reconnect the WebSocket after creating new metrics to see their events.

---

## Two-Tier Metric/Logpoint Architecture

In multi-tenant mode, each user owns their own metric at a given code location. Detrix merges these into a single shared logpoint at the debugger level.

```
User A: metric{line:42, exprs:[x, y]}  ─┐
                                          ├─▶ shared logpoint{line:42, exprs:[x, y, z]}
User B: metric{line:42, exprs:[y, z]}  ─┘
```

**Storage:** One metric per `(location, connection_id, user_id)`. Each user owns their own metric entity. System metrics (no user) use an internal `__system__` sentinel in the database to ensure the unique index works correctly.

**DAP:** One logpoint per `(file, line)` per connection. Expressions from all enabled metrics at that location are unioned and deduplicated.

**Lifecycle:** When a user adds, removes, enables, or disables a metric, Detrix re-syncs the logpoint by collecting all enabled metrics at that location, merging their expressions, and updating the debugger. If no enabled metrics remain at a location, the logpoint is removed.

---

## Public Endpoints

Some endpoints are exempt from authentication even when auth is enabled:

```toml
# Default public endpoints (configurable)
[api.auth]
public_endpoints = ["/health", "/status", "/metrics", "/api/health", "/api/status"]
grpc_public_methods = ["GetStatus"]
```

Additional default public endpoints (for MCP bridge lifecycle):
- `/detrix/mcp/heartbeat`
- `/detrix/mcp/disconnect`
- `/api/v1/connections/touch`

---

## Error Codes

| Code | Name | HTTP | gRPC | Description |
|------|------|------|------|-------------|
| 1008 | `INVALID_TENANT_ID` | 400 | `INVALID_ARGUMENT` | Invalid `user_id` or `agent_id` (empty, control chars, reserved pattern) |
| 6001 | `UNAUTHORIZED` | 401 | `UNAUTHENTICATED` | Missing or invalid token / JWT |
| 6002 | `FORBIDDEN` | 403 | `PERMISSION_DENIED` | Scope violation (e.g., mutating another user's metric) |

---

## Cloud Deployment with Auth

For cloud debugging with multiple users, combine auth with daemon registration:

```yaml
# docker-compose.yml
services:
  detrix:
    image: ghcr.io/flashus/detrix:latest
    ports:
      - "8090:8090"
    environment:
      DETRIX_ADVERTISE_URL: http://your-host:8090
    volumes:
      - ./detrix.toml:/data/detrix/detrix.toml:ro
```

```toml
# detrix.toml
[api.auth]
mode = "simple"

[[api.auth.users]]
token = "dtx_alice_secret"
user_id = "alice"
role = "user"

[[api.auth.users]]
token = "dtx_bob_secret"
user_id = "bob"
role = "user"

[[api.auth.users]]
token = "dtx_admin_secret"
user_id = "admin"
role = "admin"
```

Each developer sets their token:

```bash
# Alice
DETRIX_TOKEN=dtx_alice_secret detrix mcp

# Bob
DETRIX_TOKEN=dtx_bob_secret detrix mcp
```

Or per-daemon in `~/detrix/credentials.toml`:

```toml
[targets."your-host:8090"]
token = "dtx_alice_secret"
```

### File Server Host

When debugging Docker containers, the MCP bridge runs a file server so the daemon can fetch source files from the host. The file server host can be configured via:

```bash
# CLI argument
detrix mcp --file-server-host host.docker.internal

# Environment variable
DETRIX_FILE_SERVER_HOST=host.docker.internal detrix mcp
```

The CLI argument takes priority over the environment variable.

---

## Configuration Reference

### AuthConfig

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `mode` | `"disabled"` / `"simple"` / `"external"` | absent (auto-auth) | Authentication mode |
| `users` | array of `StaticUser` | `[]` | Per-user tokens (simple mode) |
| `jwt` | `JwtConfig` | — | JWT settings (external mode) |
| `public_endpoints` | array of strings | `["/health", ...]` | Paths exempt from auth |
| `grpc_public_methods` | array of strings | `["GetStatus"]` | gRPC methods exempt from auth |

### StaticUser

| Field | Type | Description |
|-------|------|-------------|
| `token` | string | Bearer token (16-512 chars, unique across users) |
| `user_id` | string | User identity stamped on metrics (unique, max 256 chars) |
| `role` | `"admin"` / `"user"` | Access role |

### JwtConfig

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `jwks_url` | string | required | JWKS endpoint URL |
| `issuer` | string | optional | Expected `iss` claim |
| `audience` | string | optional | Expected `aud` claim |
| `cache_ttl_seconds` | u64 | `300` | JWKS key cache TTL |
| `admin_role_claim` | string | optional | JWT claim containing roles (supports dot notation) |
| `admin_role_value` | string | optional | Value that grants admin access |
