# Detrix CLI Reference

Complete reference for all `detrix` commands. Run `detrix --help` or `detrix <command> --help` for built-in help.

## Global Flags

| Flag | Short | Description |
|------|-------|-------------|
| `--config <path>` | `-c` | Config file path (default: `~/detrix/detrix.toml`) |
| `--debug` | `-d` | Enable debug logging |
| `--quiet` | `-q` | Minimal output (IDs only, for scripting) |
| `--format <fmt>` | | Output format: `table` (default), `json`, `toon` |
| `--no-color` | | Disable colored output |

---

## Setup

### `detrix init`

Create default configuration file.

```bash
detrix init                                      # ~/detrix/detrix.toml
detrix init --path /custom/path/detrix.toml      # custom location
detrix init --force                              # overwrite existing
detrix init --full                               # all options with comments
```

---

## Daemon

The daemon runs in the background, manages connections, and serves all APIs. It auto-spawns when using MCP; these commands are for manual control.

### `detrix daemon start|stop|restart|status`

```bash
detrix daemon start       # start in background
detrix daemon status      # check if running
detrix daemon stop        # graceful shutdown
detrix daemon restart     # stop + start
```

Logs: `~/detrix/log/detrix_daemon.log` (daily rotation). PID file: `~/detrix/detrix.pid`.

### `detrix ps`

List all running detrix processes.

```bash
detrix ps
```

---

## Connections

A connection links Detrix to a running debugger (debugpy, delve, lldb-dap).

### `detrix connection create`

```bash
detrix connection create --port 5678 --language python --id myconn
detrix connection create -H 192.168.1.10 -p 5678 -l go
detrix connection create -p 4711 -l rust --program ./target/debug/my_app
detrix connection create -p 5678 -l python --safe-mode   # logpoints only
```

| Flag | Short | Required | Default | Description |
|------|-------|----------|---------|-------------|
| `--port` | `-p` | Yes | | Debugger port |
| `--language` | `-l` | Yes | | `python`, `go`, or `rust` |
| `--host` | `-H` | No | `127.0.0.1` | Debugger host |
| `--id` | | No | auto-generated | Custom connection ID |
| `--program` | | No | | Binary path (Rust launch mode) |
| `--safe-mode` | | No | `false` | Only allow logpoints (non-blocking) |

### `detrix connection list`

```bash
detrix connection list
detrix connection list --active    # only connected
```

### `detrix connection get <id>`

```bash
detrix connection get myconn
```

### `detrix connection close <id>`

```bash
detrix connection close myconn
```

### `detrix connection cleanup`

Remove stale/disconnected connections.

```bash
detrix connection cleanup
```

---

## Metrics

A metric is an observation point: a location in code + one or more expressions to evaluate.

### `detrix metric add <name>`

```bash
# Single expression
detrix metric add user_login -l "@auth.py#42" -e "user.id" -C myconn

# Multiple expressions
detrix metric add order_info -l "@checkout.py#127" \
  -e "order.total" -e "order.currency" -e "len(order.items)" -C myconn

# With group
detrix metric add latency -l "@handler.go#55" -e "elapsed" -C myconn -g perf

# Replace existing metric at same location
detrix metric add user_login -l "@auth.py#42" -e "user.email" -C myconn --replace
```

| Flag | Short | Required | Default | Description |
|------|-------|----------|---------|-------------|
| `<name>` | | Yes | | Metric name (positional) |
| `--location` | `-l` | Yes | | File + line: `@file.py#42` |
| `--expressions` | `-e` | Yes | | Expression(s) to evaluate (repeatable) |
| `--connection` | `-C` | Yes | | Connection ID |
| `--group` | `-g` | No | | Group name |
| `--replace` | | No | `false` | Replace existing metric |
| `--enabled` | | No | `true` | Start enabled/disabled |

### `detrix metric list`

```bash
detrix metric list
detrix metric list --group perf      # filter by group
detrix metric list --enabled         # only enabled
```

### `detrix metric get <name>`

```bash
detrix metric get user_login
```

### `detrix metric remove <name>`

```bash
detrix metric remove user_login
```

### `detrix metric enable|disable <name>`

```bash
detrix metric enable user_login
detrix metric disable user_login
```

---

## Groups

Groups organize metrics for bulk operations.

### `detrix group list`

```bash
detrix group list
```

### `detrix group enable|disable <name>`

```bash
detrix group enable perf
detrix group disable perf
```

### `detrix group metrics <name>`

List all metrics in a group.

```bash
detrix group metrics perf
```

---

## Events

Events are captured values when observation points fire.

### `detrix event query`

```bash
detrix event query                          # all events (last 100)
detrix event query --metric user_login      # filter by metric
detrix event query -m user_login -l 20      # limit results
```

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--metric` | `-m` | | Filter by metric name |
| `--limit` | `-l` | `100` | Max events to return |

### `detrix event latest <metric>`

Get the most recent event for a metric.

```bash
detrix event latest user_login
```

---

## System Control

### `detrix status`

```bash
detrix status              # summary
detrix status --verbose    # include connections and metrics
```

### `detrix wake` / `detrix sleep`

Control the observing state. Sleep pauses all connections (zero overhead); wake resumes.

```bash
detrix wake
detrix sleep
```

---

## Diagnostics

### `detrix validate <expression>`

Check if an expression is safe to evaluate.

```bash
detrix validate "user.id" -l python
detrix validate "len(items)" -l python
detrix validate "os.system('rm -rf /')" -l python    # blocked
```

| Flag | Short | Required | Description |
|------|-------|----------|-------------|
| `--language` | `-l` | Yes | `python`, `go`, or `rust` |

### `detrix inspect <file>`

Parse a source file to discover observable variables and functions.

```bash
detrix inspect src/auth.py
detrix inspect src/auth.py --line 42
detrix inspect src/auth.py --find-variable user
```

| Flag | Short | Description |
|------|-------|-------------|
| `--line` | `-l` | Inspect specific line |
| `--find-variable` | | Search for a variable |

### `detrix diff`

Parse a git diff for print/log statements and convert them to metrics.

```bash
git diff | detrix diff --stdin -C myconn
detrix diff -f changes.patch -C myconn -g debug
detrix diff --stdin --dry-run                      # preview only
```

| Flag | Short | Description |
|------|-------|-------------|
| `--stdin` | | Read diff from stdin |
| `--file` | `-f` | Read diff from file |
| `--connection` | `-C` | Connection ID for created metrics |
| `--group` | `-g` | Group name for created metrics |
| `--dry-run` | | Show what would be created |

---

## Configuration

### `detrix config get`

```bash
detrix config get                        # full config
detrix config get --key storage.path     # specific key
```

### `detrix config update`

```bash
detrix config update --toml '[api.rest]\nport = 9090'
detrix config update -f new_config.toml
detrix config update --stdin --merge          # merge from stdin
detrix config update --toml '...' --persist   # save to disk
```

| Flag | Short | Description |
|------|-------|-------------|
| `--toml` | | TOML string |
| `--file` | `-f` | Read from file |
| `--stdin` | | Read from stdin |
| `--merge` | | Merge with existing (default: replace) |
| `--persist` | | Write changes to disk |

### `detrix config validate`

```bash
detrix config validate                     # validate current config file
detrix config validate -f other.toml       # validate specific file
detrix config validate --content '...'     # validate TOML string via daemon
```

### `detrix config reload`

Reload configuration from disk without restarting.

```bash
detrix config reload
```

---

## MCP Server

### `detrix mcp`

Start MCP server for LLM integration (Claude Code, Cursor, Windsurf).

```bash
detrix mcp                          # bridge mode (default, connects to daemon)
detrix mcp --no-daemon              # direct mode (standalone, for testing)
detrix mcp --daemon-port 9090       # custom daemon port
```

Normally configured in `.mcp.json`, not run manually. See [INSTALL.md](INSTALL.md).

---

## Rust Debugging

### `detrix lldb-serve <program>`

Start an LLDB DAP server wrapping lldb-dap for Rust debugging. Use when lldb-dap doesn't support `--connection` flag natively.

```bash
detrix lldb-serve ./target/debug/my_app
detrix lldb-serve ./target/debug/my_app --listen 127.0.0.1:5678
detrix lldb-serve ./target/debug/my_app --stop-on-entry
detrix lldb-serve ./target/debug/my_app --persist -- --arg1 --arg2
```

| Flag | Default | Description |
|------|---------|-------------|
| `--listen` | `127.0.0.1:4711` | Listen address |
| `--stop-on-entry` | `false` | Pause at program entry |
| `--persist` | `false` | Keep running after client disconnects |
| `--lldb-dap-path` | auto-detected | Path to lldb-dap binary |
| `--cwd` | | Working directory for debugged program |
| `--` | | Program arguments (trailing) |

---

## System Events & Usage

### `detrix system-event query`

Query internal system events (connections, errors, adapter events).

```bash
detrix system-event query
detrix system-event query -t error -l 20
detrix system-event query --unacked
```

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--event-type` | `-t` | `all` | `connection`, `metric`, `adapter`, `error`, `all` |
| `--limit` | `-l` | `50` | Max events |
| `--unacked` | | `false` | Only unacknowledged |

### `detrix usage`

Show MCP tool usage statistics.

```bash
detrix usage
```

---

## GELF Output

### `detrix output`

Show GELF output configuration status and test connectivity.

```bash
detrix output            # show config
detrix output --test     # test connection to Graylog
```

---

## Default Paths & Ports

| Item | Default |
|------|---------|
| Config file | `~/detrix/detrix.toml` |
| Database | `~/detrix/detrix.db` |
| Logs | `~/detrix/log/` |
| PID file | `~/detrix/detrix.pid` |
| REST/WebSocket port | 8090 |
| gRPC port | 50061 |
| LLDB DAP listen | `127.0.0.1:4711` |
