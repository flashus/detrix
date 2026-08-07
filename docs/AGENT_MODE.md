# Standalone Agent Mode

Agent mode runs a Detrix agent on a Linux host and connects it to a central
Detrix server over an authenticated gRPC stream. The agent discovers Go ELF
processes with DWARF information, performs the eBPF capture locally, and
forwards connections and events to the server.

This is a v1.3.0 feature. The current scanner path is Linux/Go/eBPF; the
normal daemon/DAP workflow remains the supported path for Python, Rust, and
local development.

## Security model

- Use a dedicated bearer token for agents. The server stores only its SHA-256
  hash; the agent reads the raw token from a file.
- Use `https://`/`grpcs://` for remote deployments and keep `verify_tls = true`.
- Configure `allowed_read_prefixes` on every production agent. The agent
  rejects startup when this list is empty unless `dev_mode = true`.
- Mount only the application binaries and source tree the agent needs. The
  server can request source-file reads through the agent stream.

## Server setup

Generate a token and its hash:

```bash
umask 077
TOKEN=$(openssl rand -hex 32)
printf '%s' "$TOKEN" > /etc/detrix/agent-token
TOKEN_HASH=$(printf '%s' "$TOKEN" | sha256sum | cut -d' ' -f1)
echo "$TOKEN_HASH"
```

Add the hash to the central server configuration:

```toml
[api.rest]
host = "0.0.0.0"
port = 8090

[api.grpc]
host = "0.0.0.0"
port = 50061

[storage]
path = "/data/detrix/data.db"

[vfs]
source_priority = ["agent", "disk"]

[agent]
agent_tokens = ["<sha256-of-agent-token>"]
# Optional compatibility floor; keep it at or below the deployed agent.
min_compatible_agent_version = "1.3.0"
```

The agent gRPC endpoint is separate from user/JWT authentication. Configure
user authentication as described in [AUTH.md](AUTH.md).

## Agent setup

Create `/etc/detrix/detrix.toml` on each observed host:

```toml
[agent]
server_grpc_url = "https://detrix.example.com:50061"
token_file = "/etc/detrix/agent-token"
agent_id_file = "/var/lib/detrix/agent-id"
verify_tls = true
metrics_host = "127.0.0.1"
metrics_port = 9091

[agent.scanner]
scan_interval_secs = 30
include_patterns = ["/srv/apps/*", "/usr/local/bin/*"]
exclude_patterns = []
require_dwarf = true
allowed_read_prefixes = ["/srv/apps", "/srv/source", "/usr/local/bin"]
```

Start and inspect the agent:

```bash
detrix --config /etc/detrix/detrix.toml agent scan --verbose
detrix --config /etc/detrix/detrix.toml agent start
detrix agent status --metrics-url http://127.0.0.1:9091
```

`agent start` reconnects with backoff and persists its ID. The connection
identity is based on hostname, language, workspace, and binary path, so a
replacement agent on the same host retains existing metrics without merging
same-named binaries from different directories.

## Docker example

From the repository root:

```bash
docker compose --env-file fixtures/docker/images.env \
  -f fixtures/docker/docker-compose.agent.yml up -d --build
docker compose -f fixtures/docker/docker-compose.agent.yml logs -f detrix-agent
```

The checked-in example uses `fixtures/docker/agent-test-token` and is for local
testing only. Replace the token, enable TLS, and narrow the read prefixes for
any real deployment.

## Health and troubleshooting

- `GET /health` on the agent metrics endpoint confirms the local process is up.
- `GET /metrics` exposes connection, event, heartbeat, and dropped-event
  counters for Prometheus.
- `agent scan --verbose` helps distinguish missing permissions, missing DWARF,
  and scanner pattern mismatches.
- If registration is rejected, check the raw token, the server hash, and
  `min_compatible_agent_version`.
- The agent needs Linux eBPF permissions (`CAP_BPF`, `CAP_PERFMON`, and often
  `SYS_PTRACE`) and visibility of the target processes, commonly via
  `pid: host` in Docker.
