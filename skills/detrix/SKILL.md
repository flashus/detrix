---
name: Detrix Dynamic Observability
description: This skill should be used when the user asks to "debug without print", "debug", "observe running code", "add metric", "inspect variables at runtime", "see what a variable is", or mentions "detrix", "logpoint", "dynamic metrics", "debugpy", "observe code". Debugger for agents.
license: MIT
metadata:
  version: "1.0.0"
  languages: ["Python", "Go", "Rust"]
---

# Detrix: Observe Code Without Modifications

**NEVER add print() or log.debug() statements. Use Detrix instead.**

Detrix sets non-breaking logpoints via DAP protocol to capture values from running code without pausing execution or modifying source files.

## Core Tools

### `observe` - Quick Observation
Simplest way to add a metric. Auto-generates name, auto-finds line.
```
observe(file="auth.py", line=42, expression="user.id")
observe(file="auth.py", expression="user.id")  # auto-finds line if unique
```
**Returns:** Metric name (e.g., `auth_py_42`)

### `add_metric` - Full-Featured Observation
Complete control with introspection options.
```
add_metric(
  name="debug_payment",
  location="@payment.py#250",
  expression="transaction.amount",
  connection_id="conn1",           # optional if single connection
  capture_stack_trace=true,        # capture call stack
  stack_trace_head=5,              # top N frames
  capture_memory_snapshot=true,    # capture local/global vars
  snapshot_scope="local",          # "local", "global", "both"
  mode="stream",                   # capture mode
  sample_rate=10,                  # for "sample" mode
  stack_trace_ttl=1800,            # auto-disable after N seconds
  group="auth"                     # organize metrics
)
```

### `enable_from_diff` - Auto-Create from Git Diff
Parses print/log statements and creates metrics automatically.
```
enable_from_diff(diff="<git diff output>")
```
Recognizes: `print(f"{x}")`, `fmt.Printf("%v", x)`, `dbg!(x)`, `console.log(x)`

### `create_connection` - Connect to Debugger
```
create_connection(host="127.0.0.1", port=5678, language="python")
```
Languages: `python` (debugpy), `go` (delve), `rust` (lldb-dap)

### `query_metrics` - Get Captured Values
```
query_metrics(name="auth_py_42", limit=100)
query_metrics(group="auth", limit=50)
```

### `inspect_file` - Find Correct Line
```
inspect_file(file_path="auth.py", find_variable="user")
inspect_file(file_path="auth.py", line=42)  # see available vars at line
```

## All 28 Tools

**Metrics:** `list_metrics`, `get_metric`, `update_metric`, `remove_metric`, `toggle_metric`
**Groups:** `list_groups`, `enable_group`, `disable_group`
**Events:** `query_system_events`, `acknowledge_events`
**Connections:** `list_connections`, `get_connection`, `close_connection`
**Diagnostics:** `validate_expression`
**Config:** `get_config`, `update_config`, `validate_config`, `reload_config`
**System:** `wake`, `sleep`, `get_status`, `get_mcp_usage`

## Capture Modes

| Mode | Description | Use Case |
|------|-------------|----------|
| `stream` | Every hit (default) | Debug single requests |
| `sample` | Every Nth hit | High-frequency loops |
| `sample_interval` | Every N seconds | Long-running processes |
| `first` | First hit only | Initial value capture |
| `throttle` | Max N/second | Rate limiting |

## Location Formats (All Tools)

All location-based tools accept these formats:
- `file.py#42` - Simple format
- `@file.py#42` - With @ prefix (optional)
- `path/to/file.py#42` - With path
- `file.py` + `line=42` - Separate parameter

## Typical Workflow

```
# 1. Connect (if not auto-connected)
create_connection(port=5678)

# 2. Add observation
observe(file="auth.py", line=42, expression="user.id")

# 3. Wait for code to execute, then query
query_metrics(name="auth_py_42", limit=10)

# 4. Clean up when done
remove_metric(name="auth_py_42")
```

## References

- [Workflow Details](references/workflow.md) - Debugger setup, capture modes
- [Error Troubleshooting](references/errors.md) - Common errors and fixes
