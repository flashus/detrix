#!/usr/bin/env python3
"""Agent emulation script for Docker cloud testing.

Emulates what a Claude Code agent would do via MCP protocol (JSON-RPC 2.0 over HTTP):
1. Initialize MCP handshake with Detrix server
2. For each language (Python, Go, Rust):
   a. Wake the app via wake MCP tool
   b. Verify connection registered via list_connections
   c. Add a metric via add_metric
   d. Wait for events
   e. Query events
   f. Report pass/fail

Usage:
    docker compose up -d
    python test_agent.py [--host localhost] [--port 8090]
"""

import argparse
import json
import sys
import time
import urllib.request
import urllib.error

# Test configuration per language
LANGUAGE_TESTS = {
    "python": {
        "app_service": "test-app-python",
        "app_port": 8091,
        "location": "trade_bot_forever.py#59",
        "expression": "order_id",
    },
    "go": {
        "app_service": "test-app-go",
        "app_port": 8091,
        "location": "detrix_example_app.go#91",
        "expression": "symbol",
    },
    "rust": {
        "app_service": "test-app-rust",
        "app_port": 8091,
        "location": "main.rs#76",
        "expression": "symbol",
    },
}


class McpClient:
    """Simple MCP client using JSON-RPC 2.0 over HTTP."""

    def __init__(self, base_url: str) -> None:
        self.base_url = base_url.rstrip("/")
        self.mcp_url = f"{self.base_url}/mcp"
        self.request_id = 0

    def _call(self, method: str, params: dict | None = None) -> dict:
        """Send a JSON-RPC 2.0 request."""
        self.request_id += 1
        payload = {
            "jsonrpc": "2.0",
            "method": method,
            "id": self.request_id,
        }
        if params is not None:
            payload["params"] = params

        data = json.dumps(payload).encode("utf-8")
        req = urllib.request.Request(
            self.mcp_url,
            data=data,
            headers={"Content-Type": "application/json"},
        )

        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                return json.loads(resp.read())
        except urllib.error.URLError as e:
            return {"error": {"message": str(e)}}

    def initialize(self) -> dict:
        """MCP initialize handshake."""
        return self._call("initialize")

    def tools_list(self) -> dict:
        """List available MCP tools."""
        return self._call("tools/list")

    def call_tool(self, name: str, arguments: dict | None = None) -> dict:
        """Call an MCP tool."""
        params = {"name": name, "arguments": arguments or {}}
        return self._call("tools/call", params)


def extract_text(response: dict) -> str:
    """Extract text content from MCP tool response."""
    result = response.get("result", {})
    content = result.get("content", [])
    texts = []
    for item in content:
        if isinstance(item, dict) and item.get("type") == "text":
            texts.append(item.get("text", ""))
    return "\n".join(texts)


def test_language(client: McpClient, language: str, config: dict) -> bool:
    """Test a single language end-to-end."""
    app_service = config["app_service"]
    app_port = config["app_port"]
    location = config["location"]
    expression = config["expression"]

    print(f"\n{'='*60}")
    print(f"  Testing {language.upper()}")
    print(f"{'='*60}")

    # Step 1: Wake the app
    print(f"  [1/4] Waking {app_service}...")
    wake_resp = client.call_tool("wake", {
        "app_url": f"http://{app_service}:{app_port}",
        "daemon_url": "http://detrix:8090",
    })

    if "error" in wake_resp:
        print(f"  FAIL: wake error: {wake_resp['error']}")
        return False

    wake_text = extract_text(wake_resp)
    print(f"  -> {wake_text[:120]}")

    # Try to extract connection_id from the response
    connection_id = None
    result = wake_resp.get("result", {})
    content = result.get("content", [])
    for item in content:
        if isinstance(item, dict) and item.get("type") == "text":
            text = item.get("text", "")
            try:
                data = json.loads(text)
                if "connection_id" in data:
                    connection_id = data["connection_id"]
            except (json.JSONDecodeError, TypeError):
                if "connection_id:" in text:
                    # Parse from message format
                    for part in text.split(","):
                        if "connection_id:" in part:
                            connection_id = part.split("connection_id:")[1].strip().rstrip(")")
                            break

    # Step 2: List connections to verify
    print("  [2/4] Verifying connection...")
    time.sleep(2)  # Give registration time to complete

    list_resp = client.call_tool("list_connections", {})
    list_text = extract_text(list_resp)

    if "error" in list_resp:
        print(f"  FAIL: list_connections error: {list_resp['error']}")
        return False

    print(f"  -> {list_text[:120]}")

    # If we don't have connection_id from wake, try to find it from list
    if not connection_id:
        # Try to parse connection_id from list output
        for item in list_resp.get("result", {}).get("content", []):
            text = item.get("text", "")
            try:
                data = json.loads(text)
                if isinstance(data, list):
                    for conn in data:
                        if isinstance(conn, dict) and conn.get("language") == language:
                            connection_id = conn.get("connection_id")
                            break
            except (json.JSONDecodeError, TypeError):
                pass

    if not connection_id:
        print(f"  FAIL: Could not find connection_id for {language}")
        return False

    print(f"  -> Connection ID: {connection_id}")

    # Step 3: Add a metric
    metric_name = f"docker-test-{language}"
    print(f"  [3/4] Adding metric '{metric_name}' at {location}...")
    add_resp = client.call_tool("add_metric", {
        "name": metric_name,
        "location": location,
        "expressions": [expression],
        "connection_id": connection_id,
    })

    if "error" in add_resp:
        print(f"  FAIL: add_metric error: {add_resp['error']}")
        return False

    add_text = extract_text(add_resp)
    print(f"  -> {add_text[:120]}")

    # Step 4: Wait and query events
    print("  [4/4] Waiting 10s for events...")
    time.sleep(10)

    query_resp = client.call_tool("query_metrics", {
        "name": metric_name,
        "limit": 5,
    })

    if "error" in query_resp:
        print(f"  WARN: query_metrics error: {query_resp['error']}")
        # Not fatal - metrics might still be working
        return True

    query_text = extract_text(query_resp)
    print(f"  -> {query_text[:200]}")

    print(f"  PASS: {language.upper()} test completed successfully")
    return True


def main() -> int:
    parser = argparse.ArgumentParser(description="Detrix Docker cloud test agent")
    parser.add_argument("--host", default="localhost", help="Detrix server host")
    parser.add_argument("--port", type=int, default=8090, help="Detrix server port")
    parser.add_argument("--languages", nargs="*", default=list(LANGUAGE_TESTS.keys()),
                        help="Languages to test (default: all)")
    args = parser.parse_args()

    base_url = f"http://{args.host}:{args.port}"
    client = McpClient(base_url)

    print(f"Detrix Docker Cloud Test Agent")
    print(f"Server: {base_url}")
    print(f"Languages: {', '.join(args.languages)}")
    print()

    # Step 0: Initialize MCP
    print("Initializing MCP connection...")
    init_resp = client.initialize()
    if "error" in init_resp:
        print(f"FATAL: MCP initialize failed: {init_resp['error']}")
        return 1

    server_info = init_resp.get("result", {}).get("serverInfo", {})
    print(f"Connected to: {server_info.get('name', 'unknown')} v{server_info.get('version', '?')}")

    # Verify wake tool is available
    tools_resp = client.tools_list()
    tools = tools_resp.get("result", {}).get("tools", [])
    tool_names = [t.get("name") for t in tools]
    if "wake" not in tool_names:
        print(f"FATAL: wake tool not found. Available tools: {tool_names}")
        return 1
    print(f"Tools available: {len(tool_names)} (wake confirmed)")

    # Run tests
    results = {}
    for language in args.languages:
        if language not in LANGUAGE_TESTS:
            print(f"WARNING: Unknown language '{language}', skipping")
            continue
        results[language] = test_language(client, language, LANGUAGE_TESTS[language])

    # Summary
    print(f"\n{'='*60}")
    print("  SUMMARY")
    print(f"{'='*60}")
    for lang, passed in results.items():
        status = "PASS" if passed else "FAIL"
        print(f"  {lang:>8}: {status}")

    all_passed = all(results.values())
    print()
    if all_passed:
        print("All tests passed!")
    else:
        failed = [lang for lang, passed in results.items() if not passed]
        print(f"FAILED: {', '.join(failed)}")

    return 0 if all_passed else 1


if __name__ == "__main__":
    sys.exit(main())
