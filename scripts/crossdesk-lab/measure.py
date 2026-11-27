"""通过隔离 Relay 驱动真实 Agent 和运维窗口。控制码只在父子进程之间传递。"""
import argparse
import json
import os
import re
import socket
import subprocess
import tempfile
import shutil
from pathlib import Path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--agent", type=Path, required=True)
    parser.add_argument("--viewer", type=Path, required=True)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--relay-port", type=int, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    root = args.config.resolve().parent
    identity = json.loads(args.config.read_text(encoding="utf-8-sig"))
    with tempfile.TemporaryDirectory(prefix="agent-", dir=root) as fresh, socket.socket() as listener:
        shutil.copyfile(root / "ca.pem", Path(fresh) / "ca.pem")
        listener.bind(("127.0.0.1", 0))
        listener.listen(1)
        listener.settimeout(45)
        env = dict(os.environ, REMOTEOPS_BENCH_DIR=fresh, REMOTEOPS_BENCH_CROSSDESK="1",
                   REMOTEOPS_BENCH_PARENT_PORT=str(listener.getsockname()[1]),
                   REMOTEOPS_BENCH_RELAY_PORT=str(args.relay_port),
                   REMOTEOPS_HUMAN_CONTROLLER_TOKEN=identity["token"],
                   REMOTEOPS_CONTROLLER_OWNER_ID=identity["owner"],
                   REMOTEOPS_CROSSDESK_FIXTURE_CA=str(root / "ca.pem"), REMOTEOPS_CROSSDESK_FIXTURE_TURN="1")
        agent = subprocess.Popen([args.agent.resolve(), "--ignored", "--nocapture", "isolated_latency_agent"], cwd=root, env=env,
                                 stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        try:
            channel, _ = listener.accept()
            with channel, channel.makefile("rb") as reader:
                channel.settimeout(40)
                code = reader.readline(40)
                if not code.endswith(b"\n") or not (9 <= len(code.strip()) <= 11):
                    raise RuntimeError(f"测试 Agent 配对码格式不符，收到 {len(code.strip())} 字节")
                result = subprocess.run([args.viewer.resolve(), "--relay-latency", args.output.resolve()],
                                        input=code, cwd=root, env=env, timeout=280)
                channel.sendall(b"X")
                result.check_returncode()
        finally:
            try:
                agent.wait(timeout=6)
            except subprocess.TimeoutExpired:
                agent.kill()
                agent.wait()
            if agent.stderr:
                detail=agent.stderr.read().decode(errors="replace")
                detail_zh = " ".join(re.findall(r"[\u4e00-\u9fff]{2,}",detail))
                if detail_zh: print(detail_zh[:300])
                tls=re.search(r"TLS 配置无效[：:](?:[ ]*)([A-Za-z0-9()_, .:-]{1,130})",detail)
                if tls: print("TLS · "+tls.group(1))
                for term in ("证书", "fingerprint", "failed to connect", "配置", "state", "策略", "Permission denied", "Access is denied", "connection refused", "状态文件", "relay", "权限"):
                    if term in detail:
                        print("Agent 测试退出时的错误类别 · " + term)


if __name__ == "__main__":
    main()
