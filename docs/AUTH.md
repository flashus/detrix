# Authentication & Authorization

Detrix supports multi-tenant access control with per-user identity, role-based authorization, and per-agent metric isolation.

---

## Auth Modes

| Mode | Config | Description |
|------|--------|-------------|
| **Disabled** | `mode = "disabled"` or absent | No authentication. All requests get Admin access. Default for local development. |
| **Simple** | `mode = "simple"` | Per-user static bearer tokens defined in `detrix.toml`. Each token maps to a user identity and role. |
| **External** | `mode = "external"` | JWT validation via a JWKS endpoint. For enterprise SSO (Keycloak, Auth0, etc.). |

When the `[api.auth]` section is omitted entirely, auth is disabled and the daemon operates in single-user Admin mode.

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

The JWT `sub` claim becomes the `user_id` on metrics. JWKS keys are cached for `cache_ttl_seconds` (default: 300s / 5 minutes).

**Admin role mapping:** If `admin_role_claim` and `admin_role_value` are set, the daemon checks the specified claim in the JWT. If the claim contains the configured value, the user gets Admin access. Otherwise, they get User access.

---

## Identity Model

Every metric carries two identity fields:

| Field | Source | Purpose |
|-------|--------|---------|
| `user_id` | Authenticated user (from token or JWT `sub`) | Owns the metric. Used for access control. |
| `agent_id` | MCP client name or `X-Detrix-Client-Id` header | Tracks which agent/session created the metric. |

Both are `Option<String>` — `None` for metrics created before auth was enabled or when auth is disabled.

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

---

## Two-Tier Metric/Logpoint Architecture

In multi-tenant mode, each user owns their own metric at a given code location. Detrix merges these into a single shared logpoint at the debugger level.

```
User A: metric{line:42, exprs:[x, y]}  ─┐
                                          ├─▶ shared logpoint{line:42, exprs:[x, y, z]}
User B: metric{line:42, exprs:[y, z]}  ─┘
```

**Storage:** One metric per `(location, connection_id, user_id)`. Each user owns their own metric entity.

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

---

## Configuration Reference

### AuthConfig

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `mode` | `"disabled"` / `"simple"` / `"external"` | absent (disabled) | Authentication mode |
| `users` | array of `StaticUser` | `[]` | Per-user tokens (simple mode) |
| `jwt` | `JwtConfig` | — | JWT settings (external mode) |
| `public_endpoints` | array of strings | `["/health", ...]` | Paths exempt from auth |
| `grpc_public_methods` | array of strings | `["GetStatus"]` | gRPC methods exempt from auth |

### StaticUser

| Field | Type | Description |
|-------|------|-------------|
| `token` | string | Bearer token for this user |
| `user_id` | string | User identity stamped on metrics |
| `role` | `"admin"` / `"user"` | Access role |

### JwtConfig

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `jwks_url` | string | required | JWKS endpoint URL |
| `issuer` | string | optional | Expected `iss` claim |
| `audience` | string | optional | Expected `aud` claim |
| `cache_ttl_seconds` | u64 | `300` | JWKS key cache TTL |
| `admin_role_claim` | string | optional | JWT claim containing roles |
| `admin_role_value` | string | optional | Value that grants admin access |
