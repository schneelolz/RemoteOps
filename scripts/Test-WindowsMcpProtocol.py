"""只读验证锁定 Windows-MCP 的 stdio 初始化和工具发现。"""

import argparse
import importlib.metadata
import json
import os
import queue
import subprocess
import sys
import threading
import time


def main():
    """以当前解释器启动 Windows-MCP，记录协议证据并回收进程。"""
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    version = importlib.metadata.version("windows-mcp")
    if version != "0.8.5":
        raise RuntimeError("windows-mcp package version is not 0.8.5")
    env = os.environ.copy()
    env.update(ANONYMIZED_TELEMETRY="false", WINDOWS_MCP_DISABLE_FLASH="1",
               WINDOWS_MCP_WATCHDOG="off", PYTHONIOENCODING="utf-8")
    process = subprocess.Popen(
        [sys.executable, "-m", "windows_mcp", "serve", "--transport", "stdio"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        creationflags=subprocess.CREATE_NO_WINDOW, env=env,
    )
    messages = queue.Queue(maxsize=128)

    def read():
        """限制每帧大小，将服务器消息送到有界队列。"""
        try:
            while True:
                line = process.stdout.readline(4 * 1024 * 1024 + 1)
                if not line:
                    raise RuntimeError("server stdout closed")
                if len(line) > 4 * 1024 * 1024:
                    raise RuntimeError("server frame exceeded limit")
                messages.put(json.loads(line), timeout=1)
        except Exception as exc:
            try:
                messages.put(exc, timeout=1)
            except queue.Full:
                pass

    def send(message):
        """发送一条 UTF-8 JSON-RPC 帧。"""
        process.stdin.write((json.dumps(message) + "\n").encode("utf-8"))
        process.stdin.flush()

    def request(identifier, method, params):
        """在固定总时限内匹配响应，拒绝未经授权的服务端请求。"""
        send(dict(jsonrpc="2.0", id=identifier, method=method, params=params))
        deadline = time.monotonic() + 60
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(method + " timed out")
            message = messages.get(timeout=remaining)
            if isinstance(message, Exception):
                raise message
            if message.get("jsonrpc") != "2.0":
                raise RuntimeError("invalid JSON-RPC version")
            if "method" in message:
                if "id" in message:
                    reply = dict(jsonrpc="2.0", id=message["id"])
                    if message["method"] == "ping":
                        reply["result"] = {}
                    else:
                        reply["error"] = dict(code=-32601, message="Unsupported method")
                    send(reply)
                continue
            if message.get("id") != identifier:
                raise RuntimeError("response id mismatch")
            if "error" in message:
                raise RuntimeError(method + " returned JSON-RPC error")
            return message["result"]

    evidence = dict(package_version=version, transport="stdio", initialized=False,
                    desktop_verified=False, input_sent=False)
    try:
        threading.Thread(target=read, daemon=True).start()
        result = request(1, "initialize", dict(
            protocolVersion="2025-11-25", capabilities={},
            clientInfo=dict(name="remoteops-protocol-probe", version="1.0"),
        ))
        if result.get("protocolVersion") != "2025-11-25":
            raise RuntimeError("unsupported negotiated protocol version")
        if not isinstance(result.get("capabilities", {}).get("tools"), dict):
            raise RuntimeError("server does not advertise tools")
        evidence.update(protocol_version=result["protocolVersion"],
                        server_info=result.get("serverInfo"))
        send(dict(jsonrpc="2.0", method="notifications/initialized"))
        tool_names = []
        cursor = None
        for identifier in range(2, 18):
            listing = request(identifier, "tools/list", {"cursor": cursor} if cursor else {})
            tool_names.extend(tool["name"] for tool in listing["tools"])
            cursor = listing.get("nextCursor")
            if not cursor:
                break
        if cursor:
            raise RuntimeError("tool pagination limit reached")
        if "Snapshot" not in tool_names or "Screenshot" not in tool_names:
            raise RuntimeError("expected observation tools are missing")
        evidence.update(initialized=True, tools=tool_names)
    except Exception as exc:
        evidence.update(error_type=type(exc).__name__, error=str(exc))
    finally:
        process.stdin.close()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
        evidence["child_exit_code"] = process.returncode
        with open(args.output, "w", encoding="utf-8") as output:
            json.dump(evidence, output, ensure_ascii=False, indent=2)
    print(json.dumps(evidence, ensure_ascii=False))
    return 0 if evidence["initialized"] else 1


if __name__ == "__main__":
    sys.exit(main())
