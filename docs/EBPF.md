# eBPF Logpoints on Linux

This page documents the Go eBPF path. For Rust capture profiles, bounded
DWARF layouts, explicit backend selection, and privileged validation commands,
see [Rust eBPF Agent Mode](ebpf-rust-agent.md).

Detrix supports two backends for Go observability:

| Backend | Platform | How it works | Requires |
|---------|----------|--------------|---------|
| **eBPF uprobes** | Linux | Attaches directly to the binary using kernel eBPF | Binary with DWARF info |
| **Delve/DAP** | macOS / fallback | Connects to a running `dlv` debug server | `dlv` daemon listening on a port |

On Linux, the eBPF backend is selected automatically when you create a Go connection. No debugger daemon is needed.

## Why eBPF?

- **~10–50× lower overhead** — uprobe fires in the kernel, no TCP round-trip to Delve
- **Zero pause** — execution is never suspended
- **No debugger daemon** — point at the binary path, not a host:port
- **Works in Docker** — runs inside the container alongside your app
- **Works on bare Linux** — same config, no special treatment needed

## Requirements

- Linux kernel ≥ 5.7 (for `bpf_get_ns_current_pid_tgid`, used for container PID resolution)
- Detrix daemon running as root (or with `CAP_BPF` + `CAP_PERFMON`)
- Go binary compiled with DWARF debug info (see below)

## Building Your Go Binary

The binary **must** retain DWARF debug symbols. Strip flags remove them.

```bash
# Development build — DWARF included by default
go build -o myapp ./cmd/server

# Production build with debug info preserved (recommended for Detrix)
go build -gcflags="all=-N -l" -o myapp ./cmd/server

# ❌ Don't strip — DWARF is removed
go build -ldflags="-s -w" -o myapp ./cmd/server
```

`-gcflags="all=-N -l"` disables optimizations and inlining, which gives Detrix the most accurate variable locations from DWARF. It's optional — Detrix will use whatever DWARF info is present.

## Connecting to a Go Binary via eBPF

### CLI

```bash
# On Linux — set host = path to binary, port is ignored
detrix connection create \
  --name myapp \
  --language go \
  --host /opt/myapp/bin/server \
  --workspace-root /opt/myapp/src

# On macOS — set host = Delve server address, port = Delve port
detrix connection create \
  --name myapp \
  --language go \
  --host 127.0.0.1 \
  --port 2345 \
  --workspace-root /opt/myapp/src
```

### MCP (Claude Code)

```
Create a connection for my Go app:
- name: myapp
- language: go
- host: /opt/myapp/bin/server    ← binary path on Linux
- workspace_root: /opt/myapp/src
```

### REST API

```json
POST /connections
{
  "name": "myapp",
  "language": "go",
  "host": "/opt/myapp/bin/server",
  "port": 0,
  "workspaceRoot": "/opt/myapp/src"
}
```

`port` is ignored on Linux — you can pass `0` or omit it.

## Adding Metrics

Once connected, metrics work the same as with Delve:

```
Add a metric to observe user.id at handlers/auth.go line 42
```

Or via CLI:

```bash
detrix metric add \
  --connection myapp \
  --name auth_event \
  --location "@handlers/auth.go#42" \
  --expression "user.id" \
  --expression "req.Method"
```

## Docker Setup

The Detrix daemon must be able to read the binary and attach uprobes. In Docker Compose:

```yaml
services:
  detrix:
    image: detrix/daemon:latest
    privileged: true          # needed for BPF syscalls
    pid: host                 # optional: simplifies PID resolution
    volumes:
      - /opt/myapp:/opt/myapp # share binary path with daemon
    environment:
      DETRIX_ADVERTISE_URL: http://detrix:8090
```

Or with minimal capabilities instead of `privileged: true`:

```yaml
cap_add:
  - SYS_ADMIN      # for bpf() syscall
  - SYS_PTRACE     # for process_vm_readv (memory reading)
```

The daemon reads Go heap memory via `process_vm_readv` to resolve strings, slices, and maps after the uprobe fires.

## PID Namespace Handling (Docker)

When the daemon runs inside a Docker container alongside a Go app, BPF uprobes report PIDs from the **root namespace** (host PIDs). However, `process_vm_readv` — used to read Go string/slice/map values from heap memory — requires the **container-local PID**.

Detrix resolves this automatically using `bpf_get_ns_current_pid_tgid(dev, ino, ...)` (kernel 5.7+):

1. At startup, the daemon reads `stat("/proc/self/ns/pid")` to get the dev/ino of its own PID namespace.
2. This is written into a BPF array map (`DETRIX_NS_INFO`) at load time.
3. Each uprobe event uses `bpf_get_ns_current_pid_tgid(dev, ino, ...)` to capture the PID as seen inside the daemon's namespace — this is what `process_vm_readv` expects.

On bare Linux (no containers), the daemon's namespace IS the root namespace, so the PID returned is identical to `bpf_get_current_pid_tgid() >> 32`. No configuration needed.

## Supported Go Types

The eBPF backend uses DWARF debug info to locate variables and reads their values from heap memory:

| Go type | Captured as |
|---------|-------------|
| `int`, `uint`, `int64`, etc. | Numeric scalar |
| `float32`, `float64` | Numeric scalar |
| `bool` | Boolean |
| `string` | String (heap read via `process_vm_readv`) |
| `[]T` (slice) | Array of elements |
| `map[K]V` | Object with key-value entries |
| struct | Object with field names and values |
| pointer to struct | Dereferenced to struct fields |

Map iteration supports both Go runtime layouts:
- **Swiss Table** (Go 1.24+): table-based layout with groups/slots
- **Classic hmap** (Go < 1.24): bucket-based layout with `bmap` chains

## Troubleshooting

**"Binary not found"** — The `host` field must be an absolute path to the ELF binary visible from the daemon's filesystem.

**"No DWARF info"** — Binary was built with `-ldflags="-s -w"`. Rebuild without strip flags.

**"process_vm_readv failed"** — Daemon lacks `SYS_PTRACE` capability. Add it to the container or run with `privileged: true`.

**"bpf_get_ns_current_pid_tgid failed"** — Kernel older than 5.7. Upgrade the kernel or use Delve/DAP.

**Variables show as `<null>`** — Variable may be optimized away. Rebuild with `-gcflags="all=-N -l"`.

**Map shows empty** — Concurrent modification during capture, or unsupported map type. File an issue with the Go version and map type.

## Event Drop Monitoring

Each eBPF probe has a per-CPU drop counter (`DETRIX_DROP_CNT`) that tracks how many events were discarded because the ring buffer was full. This happens when the application produces events faster than the daemon can consume them — typically under extreme load or when many logpoints fire on hot paths.

**Checking drops via REST API:**

```bash
# Query drop count for a specific metric (by metric ID)
curl http://localhost:8090/api/v1/metrics/{id}/drop-count
```

**Example response:**
```json
{
  "metricId": 123,
  "metricName": "my_metric",
  "dropCount": 0,
  "explanation": "No events dropped — ring buffer is keeping up"
}
```

**Interpreting results:**
- `0` — No drops, ring buffer is keeping up
- Increasing count — Ring buffer is overflowing; consider reducing the number of active logpoints or moving them off hot paths

The drop counter is a per-CPU array internally, summed when read. This avoids lock contention in the BPF program — each CPU increments its own counter without atomics.

## Comparison: eBPF vs Delve/DAP

| | eBPF (Linux) | Delve/DAP (macOS / fallback) |
|-|--------------|------------------------------|
| Overhead | ~microseconds | ~milliseconds |
| Pauses execution? | No | No (logpoints); Yes (~1ms for complex expressions) |
| Requires running debugger? | No | Yes (`dlv` listening on a port) |
| Connection target | Binary path | host:port of `dlv` server |
| Container support | Built-in | Via port-forwarding or `--network host` |
| Kernel requirement | ≥ 5.7 | None |
| Full expression eval | Variable reads only | Full Go expression evaluator |

For **production Linux deployments**, eBPF is strongly preferred. For **macOS development** or when full expression evaluation is needed, use Delve/DAP.
