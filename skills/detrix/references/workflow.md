# Detrix Workflow Guide

## Starting Debug Sessions

### Python (debugpy)
```bash
# Option 1: Wait for client
python -m debugpy --listen 127.0.0.1:5678 --wait-for-client app.py

# Option 2: Don't wait (app runs immediately)
python -m debugpy --listen 127.0.0.1:5678 app.py
```
Then connect:
```
create_connection(host="127.0.0.1", port=5678, language="python")
```

### Go (delve)
```bash
# Debug mode
dlv debug --headless --listen=:2345 --api-version=2

# Attach to running process
dlv attach <pid> --headless --listen=:2345 --api-version=2
```
Then connect:
```
create_connection(host="127.0.0.1", port=2345, language="go")
```

### Rust (lldb-dap)
```bash
lldb-dap --port 5678
```
Then connect:
```
create_connection(host="127.0.0.1", port=5678, language="rust")
```

## Finding the Right Line

### Find where variable is defined
```
inspect_file(file_path="auth.py", find_variable="user")
```
Returns lines where `user` appears with context.

### See available variables at line
```
inspect_file(file_path="auth.py", line=42)
```
Returns variables in scope at that line.

### Validate expression before adding
```
validate_expression(language="python", expression="user.email")
```
Checks for unsafe operations (eval, file I/O, etc).

## Capture Modes Explained

### `stream` (default)
Captures every hit. Best for debugging specific requests.
```
add_metric(mode="stream", ...)
```

### `sample`
Captures every Nth hit. Best for high-frequency loops.
```
add_metric(mode="sample", sample_rate=10, ...)  # every 10th
```

### `sample_interval`
Captures every N seconds. Best for long-running processes.
```
add_metric(mode="sample_interval", sample_interval=5, ...)  # every 5 sec
```

### `first`
Captures only first hit then auto-disables.
```
add_metric(mode="first", ...)
```

### `throttle`
Maximum N captures per second.
```
add_metric(mode="throttle", throttle_rate=10, ...)  # max 10/sec
```

## Introspection Features

### Stack Traces
Capture call stack when metric fires.
```
add_metric(
  capture_stack_trace=true,
  stack_trace_head=5,      # top 5 frames
  stack_trace_tail=2,      # bottom 2 frames
  ...
)
```

### Memory Snapshots
Capture variable values at metric location.
```
add_metric(
  capture_memory_snapshot=true,
  snapshot_scope="local",   # "local", "global", "both"
  ...
)
```

### Auto-Cleanup (TTL)
Auto-disable metric after N seconds. Prevents forgotten debug metrics.
```
add_metric(stack_trace_ttl=900, ...)  # 15 minutes
```

## Organizing Metrics

### Groups
Organize related metrics for bulk operations.
```
add_metric(group="auth", ...)
add_metric(group="auth", ...)

# Enable/disable all auth metrics
enable_group(name="auth")
disable_group(name="auth")

# Query all auth metrics
query_metrics(group="auth")
```

### List and Filter
```
list_metrics()                    # all metrics
list_metrics(group="auth")        # by group
list_metrics(enabled_only=true)   # only enabled
```

## Common Patterns

### Debug a specific request
```
observe(file="handler.py", line=42, expression="request.id")
# trigger the request
query_metrics(name="handler_py_42", limit=1)
```

### Monitor loop variable
```
add_metric(
  name="loop_item",
  location="@process.py#100",
  expression="item.value",
  mode="sample",
  sample_rate=100  # every 100th iteration
)
```

### Temporary deep debugging
```
add_metric(
  name="deep_debug",
  location="@core.py#55",
  expression="state",
  capture_stack_trace=true,
  capture_memory_snapshot=true,
  stack_trace_ttl=600  # auto-disable in 10 min
)
```
