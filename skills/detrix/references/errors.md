# Detrix Error Reference

## Location Errors

### "Could not determine line number"
**Problem:** No line number provided.
```
# Wrong - no line
add_metric(location="auth.py", ...)

# Correct - line in location
add_metric(location="auth.py#127", ...)

# Correct - separate line parameter
add_metric(location="auth.py", line=127, ...)
```
**Note:** @ prefix is optional. Both `auth.py#127` and `@auth.py#127` work.

### "Invalid line number"
**Problem:** Line number must be positive integer.
```
# Wrong
add_metric(location="auth.py#0", ...)
add_metric(location="auth.py#abc", ...)

# Correct
add_metric(location="auth.py#127", ...)
```

### "File not found"
**Problem:** Path doesn't exist or isn't accessible.
```
# Use inspect_file to verify path
inspect_file(file_path="auth.py")
```

## Connection Errors

### "Connection not found"
**Problem:** No connection exists with that ID.
**Solution:** Create connection first:
```
create_connection(host="127.0.0.1", port=5678, language="python")
```
Use returned `connection_id` in `add_metric`.

### "Connection is not connected"
**Problem:** Connection was lost or debugger stopped.
**Solutions:**
1. Check debugger is still running: `ps aux | grep debugpy`
2. View connection status: `list_connections()`
3. Reconnect: `create_connection(...)`

### "No active connections"
**Problem:** Trying to use tool without any connection.
**Solution:** Create connection first, or check if debugger is running.

### "Connection refused"
**Problem:** Debugger not listening on specified port.
**Solutions:**
1. Verify debugger started: `lsof -i :5678`
2. Check correct port
3. Ensure `--wait-for-client` if using debugpy

## Query Errors

### "'name' or 'group' required"
**Problem:** `query_metrics` needs a filter.
```
# Wrong
query_metrics()

# Correct
query_metrics(name="my_metric")
query_metrics(group="auth")
```

### "Metric not found"
**Problem:** Metric with that name doesn't exist.
```
# List all metrics to see names
list_metrics()
```

## Safety Errors

### "Unsafe expression"
**Problem:** Expression contains forbidden operations.

**Forbidden operations:**
- Code execution: `eval()`, `exec()`, `compile()`
- File I/O: `open()`, `read()`, `write()`
- Imports: `__import__`, `importlib`
- System calls: `os.system()`, `subprocess`
- Network: `socket`, `requests`

**Safe expressions:**
- Attribute access: `user.id`, `request.headers`
- Method calls on safe types: `str(value)`, `len(items)`
- Dict/list access: `data["key"]`, `items[0]`
- Simple operations: `x + y`, `a and b`

### "Expression validation failed"
**Problem:** Syntax error in expression.
```
# Validate before adding
validate_expression(language="python", expression="user.id")
```

## No Events Captured

**Symptoms:** `query_metrics` returns empty results.

**Debugging steps:**
```
# 1. Check metric exists and is enabled
list_metrics(name="my_metric")

# 2. Get detailed metric info
get_metric(name="my_metric")

# 3. Verify connection is active
list_connections()

# 4. Check system status
get_status()
```

**Common causes:**
1. **Metric disabled** - Enable with `toggle_metric(name="...", enabled=true)`
2. **Code path not hit** - Ensure the line actually executes
3. **Wrong line number** - Use `inspect_file` to find correct line
4. **Debugger disconnected** - Reconnect with `create_connection`
5. **Expression error** - Check debugger logs for evaluation errors

## System Errors

### "Daemon not running"
**Problem:** Detrix daemon isn't started.
**Solution:** Daemon auto-starts with MCP. If manual: `detrix serve --daemon`

### "Rate limited"
**Problem:** Too many requests in short time.
**Solution:** Wait and retry. Check `get_status()` for limits.

### "Configuration error"
**Problem:** Invalid config value.
```
# Validate before applying
validate_config(config="<toml>")

# Get current config
get_config()
```
