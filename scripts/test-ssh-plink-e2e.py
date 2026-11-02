#!/usr/bin/env python3
"""仅本机的真实 SSH/Plink 测试夹具；不运行命令解释器，不使用现场凭据。

安装见 third-party/putty/README.md。serve 的 stdin/stdout 为 NDJSON；第一条
stdin 为 {"password": <父进程生成的随机合成密码>}，stdout 永不返回密码。
"""

from __future__ import annotations

import argparse
import base64
import contextlib
import ctypes
from ctypes import wintypes
import hashlib
import hmac
import json
import logging
import os
from pathlib import Path
import secrets
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time

import paramiko

PARAMIKO_VERSION = "4.0.0"
PLINK_HASH = "50479953865b30775056441b10fdcb984126ba4f98af4f64756902a807b453e7"
USERNAME = "remoteops-fixture"
FIRST = "FIXTURE-FIRST 中文 😀\n".encode()
FINAL = b"FIXTURE-FINAL\n"
STDERR = b"FIXTURE-STDERR\n"
COMMANDS = {b"fixture-stream", b"fixture-reject", b"fixture-timeout"}
logging.getLogger("paramiko").addHandler(logging.NullHandler())
logging.getLogger("paramiko").propagate = False


def emit(value: dict) -> None:
    print(json.dumps(value, ensure_ascii=True), flush=True)


class FixtureServer:
    """仅接受固定测试用户名、密码和白名单 exec 的回环 SSH 服务。"""

    def __init__(self, password: str):
        if not isinstance(password, str) or not 16 <= len(password) <= 256:
            raise ValueError("synthetic_password_length")
        if any(c in password for c in "\r\n\0"):
            raise ValueError("synthetic_password_characters")
        self.password = password
        self.key = paramiko.ECDSAKey.generate()
        self.lock = threading.Lock()
        self.stopping = threading.Event()
        self.transports: set[paramiko.Transport] = set()
        self.counts = {name: 0 for name in (
            "connections", "auth_accepted", "auth_rejected", "exec_accepted",
            "exec_rejected", "shell_rejected", "streams_completed", "timeout_peer_closed", "protocol_errors",
        )}
        self.listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen(16)
        self.listener.settimeout(0.2)
        self.port = self.listener.getsockname()[1]
        self.thread = threading.Thread(target=self._accept, daemon=True)
        self.thread.start()

    def count(self, name: str) -> None:
        with self.lock:
            self.counts[name] += 1

    def metadata(self, event: str) -> dict:
        with self.lock:
            key = self.key
            stats = dict(self.counts)
        digest = base64.b64encode(hashlib.sha256(key.asbytes()).digest()).decode().rstrip("=")
        return {"event": event, "host": "127.0.0.1", "port": self.port,
                "username": USERNAME, "algorithm": key.get_name(),
                "public_key": key.get_base64(), "fingerprint": "SHA256:" + digest,
                "paramiko_version": paramiko.__version__, "stats": stats}

    def rotate(self) -> dict:
        new_key = paramiko.ECDSAKey.generate()
        with self.lock:
            self.key = new_key
        return self.metadata("rotated")

    def _accept(self) -> None:
        while not self.stopping.is_set():
            try:
                connection, peer = self.listener.accept()
            except socket.timeout:
                continue
            except OSError:
                return
            with self.lock:
                capacity = len(self.transports) < 16
            if peer[0] != "127.0.0.1" or not capacity:
                connection.close()
                continue
            threading.Thread(target=self._serve, args=(connection,), daemon=True).start()

    def _serve(self, connection: socket.socket) -> None:
        transport = None
        try:
            connection.settimeout(10)
            transport = paramiko.Transport(connection)
            transport.banner_timeout = 5
            transport.auth_timeout = 5
            with self.lock:
                key = self.key
                self.transports.add(transport)
            self.count("connections")
            transport.add_server_key(key)
            transport.start_server(server=FixtureInterface(self))
            while transport.is_active() and not self.stopping.wait(0.05):
                # 保留 channel 强引用；exec 回调的输出线程持有实际使用的 channel。
                channel = transport.accept(0.1)
                if channel is not None:
                    while not channel.closed and transport.is_active() and not self.stopping.wait(0.05):
                        pass
        except Exception:
            # 第三方协议异常只计数，不把异常对象写入父 runner 的日志。
            self.count("protocol_errors")
        finally:
            if transport is not None:
                transport.close()
                with self.lock:
                    self.transports.discard(transport)
            connection.close()

    def output(self, channel: paramiko.Channel, command: bytes) -> None:
        try:
            channel.sendall(FIRST)
            if command == b"fixture-timeout":
                while channel.get_transport().is_active() and not channel.closed and not self.stopping.wait(0.05):
                    pass
                if not self.stopping.is_set():
                    self.count("timeout_peer_closed")
                return
            if self.stopping.wait(1.2):
                return
            channel.sendall_stderr(STDERR)
            channel.sendall(FINAL)
            channel.send_exit_status(0)
            channel.shutdown_write()
            self.count("streams_completed")
        except Exception:
            pass
        finally:
            channel.close()

    def close(self) -> None:
        self.stopping.set()
        self.listener.close()
        with self.lock:
            transports = list(self.transports)
        for transport in transports:
            transport.close()
        self.thread.join(2)


class FixtureInterface(paramiko.ServerInterface):
    def __init__(self, fixture: FixtureServer):
        self.fixture = fixture

    def get_allowed_auths(self, username):
        return "password"

    def check_auth_password(self, username, password):
        accepted = username == USERNAME and hmac.compare_digest(
            password.encode("utf-8"), self.fixture.password.encode("utf-8"))
        self.fixture.count("auth_accepted" if accepted else "auth_rejected")
        return paramiko.AUTH_SUCCESSFUL if accepted else paramiko.AUTH_FAILED

    def check_channel_request(self, kind, chanid):
        return paramiko.OPEN_SUCCEEDED if kind == "session" else paramiko.OPEN_FAILED_ADMINISTRATIVELY_PROHIBITED

    def check_channel_exec_request(self, channel, command):
        if command not in COMMANDS or command == b"fixture-reject":
            self.fixture.count("exec_rejected")
            return False
        self.fixture.count("exec_accepted")
        threading.Thread(target=self.fixture.output, args=(channel, command), daemon=True).start()
        return True

    def check_channel_shell_request(self, channel):
        self.fixture.count("shell_rejected")
        return False


def serve() -> int:
    # 不把输入行或异常详情写入 stdout/stderr，避免父进程误记合成密码。
    fixture = None
    try:
        first = sys.stdin.buffer.readline(4097)
        if len(first) > 4096:
            raise ValueError("input_limit")
        fixture = FixtureServer(json.loads(first)["password"])
        emit(fixture.metadata("ready"))
        while line := sys.stdin.buffer.readline(4097):
            if len(line) > 4096:
                raise ValueError("input_limit")
            action = json.loads(line).get("action")
            if action == "rotate_key":
                emit(fixture.rotate())
            elif action == "stats":
                emit(fixture.metadata("stats"))
            elif action == "stop":
                break
            else:
                raise ValueError("unsupported_action")
        return 0
    except (ValueError, KeyError, TypeError):
        emit({"event": "error", "reason": "invalid_fixture_input"})
        return 2
    finally:
        if fixture is not None:
            fixture.close()


def private_file(path: Path, content: bytes) -> None:
    """CREATE_NEW 与受保护的当前用户 DACL 同时生效，没有先公开再收紧的窗口。"""
    if os.name != "nt":
        raise RuntimeError("windows_required")
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    advapi = ctypes.WinDLL("advapi32", use_last_error=True)
    kernel.GetCurrentProcess.restype = wintypes.HANDLE
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    advapi.OpenProcessToken.argtypes = [wintypes.HANDLE, wintypes.DWORD, ctypes.POINTER(wintypes.HANDLE)]
    advapi.GetTokenInformation.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p,
                                         wintypes.DWORD, ctypes.POINTER(wintypes.DWORD)]
    advapi.ConvertSidToStringSidW.argtypes = [ctypes.c_void_p, ctypes.POINTER(wintypes.LPWSTR)]
    advapi.ConvertStringSecurityDescriptorToSecurityDescriptorW.argtypes = [
        wintypes.LPCWSTR, wintypes.DWORD, ctypes.POINTER(ctypes.c_void_p), ctypes.POINTER(wintypes.DWORD)]
    kernel.LocalFree.argtypes = [ctypes.c_void_p]
    token = wintypes.HANDLE()
    if not advapi.OpenProcessToken(kernel.GetCurrentProcess(), 8, ctypes.byref(token)):
        raise ctypes.WinError(ctypes.get_last_error())
    sid_text = wintypes.LPWSTR()
    descriptor = ctypes.c_void_p()
    created = False
    try:
        size = wintypes.DWORD()
        advapi.GetTokenInformation(token, 1, None, 0, ctypes.byref(size))
        user = ctypes.create_string_buffer(size.value)
        if not advapi.GetTokenInformation(token, 1, user, size, ctypes.byref(size)):
            raise ctypes.WinError(ctypes.get_last_error())
        sid_pointer = ctypes.cast(user, ctypes.POINTER(ctypes.c_void_p))[0]
        if not advapi.ConvertSidToStringSidW(sid_pointer, ctypes.byref(sid_text)):
            raise ctypes.WinError(ctypes.get_last_error())
        sddl = f"D:P(A;;FA;;;{sid_text.value})"
        if not advapi.ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl, 1, ctypes.byref(descriptor), None):
            raise ctypes.WinError(ctypes.get_last_error())

        class SecurityAttributes(ctypes.Structure):
            _fields_ = [("length", wintypes.DWORD), ("descriptor", ctypes.c_void_p), ("inherit", wintypes.BOOL)]

        attrs = SecurityAttributes(ctypes.sizeof(SecurityAttributes), descriptor, False)
        kernel.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD,
                                      ctypes.POINTER(SecurityAttributes), wintypes.DWORD,
                                      wintypes.DWORD, wintypes.HANDLE]
        kernel.CreateFileW.restype = wintypes.HANDLE
        handle = kernel.CreateFileW(str(path), 0x40000000, 0, ctypes.byref(attrs), 1, 0x80, None)
        if handle == ctypes.c_void_p(-1).value:
            raise ctypes.WinError(ctypes.get_last_error())
        created = True
        try:
            kernel.WriteFile.argtypes = [wintypes.HANDLE, ctypes.c_void_p, wintypes.DWORD,
                                        ctypes.POINTER(wintypes.DWORD), ctypes.c_void_p]
            written = wintypes.DWORD()
            if not kernel.WriteFile(handle, content, len(content), ctypes.byref(written), None) or written.value != len(content):
                raise OSError("fixture_credential_write")
        finally:
            kernel.CloseHandle(handle)
        # 读取实际文件 DACL 作独立核验，既检查保护位，也检查只有该用户 ACE。
        advapi.GetFileSecurityW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD,
                                           ctypes.c_void_p, wintypes.DWORD, ctypes.POINTER(wintypes.DWORD)]
        security_size = wintypes.DWORD()
        advapi.GetFileSecurityW(str(path), 4, None, 0, ctypes.byref(security_size))
        security = ctypes.create_string_buffer(security_size.value)
        if not advapi.GetFileSecurityW(str(path), 4, security, security_size, ctypes.byref(security_size)):
            raise ctypes.WinError(ctypes.get_last_error())
        actual_sddl = wintypes.LPWSTR()
        advapi.ConvertSecurityDescriptorToStringSecurityDescriptorW.argtypes = [
            ctypes.c_void_p, wintypes.DWORD, wintypes.DWORD, ctypes.POINTER(wintypes.LPWSTR), ctypes.c_void_p]
        if not advapi.ConvertSecurityDescriptorToStringSecurityDescriptorW(security, 1, 4, ctypes.byref(actual_sddl), None):
            raise ctypes.WinError(ctypes.get_last_error())
        try:
            if actual_sddl.value != sddl:
                raise RuntimeError("fixture_credential_acl_mismatch")
        finally:
            kernel.LocalFree(actual_sddl)
    except BaseException:
        if created:
            path.unlink(missing_ok=True)
        raise
    finally:
        kernel.CloseHandle(token)
        if sid_text:
            kernel.LocalFree(sid_text)
        if descriptor:
            kernel.LocalFree(descriptor)


@contextlib.contextmanager
def password_file(root: Path, password: str, cleanup: list[bool]):
    path = root / ("凭据 " + secrets.token_hex(8) + ".tmp")
    private_file(path, password.encode("utf-8"))
    try:
        yield path
    finally:
        deadline = time.monotonic() + 0.5
        while True:
            try:
                path.unlink(missing_ok=True)
                break
            except OSError as error:
                if error.winerror not in (32, 33) or time.monotonic() >= deadline:
                    raise
                time.sleep(0.01)
        cleanup.append(not path.exists())


def run_plink(binary: Path, fixture: FixtureServer, pin: str, credential: Path,
              command: str, stop_reason: str | None = None) -> dict:
    arguments = [str(binary), "-load", "RemoteOpsFixtureMissing-" + secrets.token_hex(16),
                 "-ssh", "-2", "-batch", "-noagent", "-noshare", "-no-trivial-auth",
                 "-no-antispoof", "-T", "-a", "-x", "-P", str(fixture.port),
                 "-l", USERNAME, "-hostkey", pin, "-pwfile", str(credential),
                 "127.0.0.1", command]
    started = time.monotonic()
    process = subprocess.Popen(arguments, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, creationflags=subprocess.CREATE_NO_WINDOW)
    output = {"stdout": bytearray(), "stderr": bytearray(), "first_stdout_ms": None}

    def read(pipe, name):
        while chunk := pipe.read1(8192):
            if name == "stdout" and output["first_stdout_ms"] is None:
                output["first_stdout_ms"] = round((time.monotonic() - started) * 1000)
            output[name].extend(chunk)

    readers = [threading.Thread(target=read, args=(process.stdout, "stdout"), daemon=True),
               threading.Thread(target=read, args=(process.stderr, "stderr"), daemon=True)]
    for reader in readers:
        reader.start()
    cancelled = False
    timed_out = False
    try:
        if stop_reason is not None:
            # 必须先收到真实 SSH 首帧，再测取消，不能把未连通当作成功取消。
            deadline = time.monotonic() + 6
            while FIRST not in output["stdout"] and process.poll() is None and time.monotonic() < deadline:
                time.sleep(0.01)
            if FIRST in output["stdout"]:
                if stop_reason == "timeout":
                    try:
                        process.wait(timeout=0.25)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        timed_out = True
                else:
                    # 显式取消与超时看门狗各走一条真实进程终止路径。
                    threading.Event().wait(0.25)
                    process.kill()
                    cancelled = True
        process.wait(timeout=8)
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=3)
        for reader in readers:
            reader.join(2)
        if any(reader.is_alive() for reader in readers):
            raise RuntimeError("fixture_reader_not_stopped")
        process.stdout.close()
        process.stderr.close()
    output.update(exit_code=process.returncode, cancelled=cancelled, timed_out=timed_out,
                  duration_ms=round((time.monotonic() - started) * 1000))
    return output


def matrix(args) -> int:
    if os.name != "nt":
        raise RuntimeError("windows_required")
    source = Path(args.plink).resolve(strict=True)
    if hashlib.sha256(source.read_bytes()).hexdigest() != PLINK_HASH:
        raise ValueError("plink_hash_mismatch")
    temp_parent = (Path(os.environ["LOCALAPPDATA"]) / "Temp").resolve(strict=True)
    if temp_parent.drive.lower() != "c:":
        raise ValueError("c_temp_required")
    root = Path(tempfile.mkdtemp(prefix="RemoteOps-SSH-中文 空格-", dir=temp_parent)).resolve()
    cleanup: list[bool] = []
    checks: list[dict] = []
    fixture = FixtureServer(secrets.token_urlsafe(32))
    public = fixture.metadata("ready")

    def check(name, passed):
        checks.append({"name": name, "passed": bool(passed)})

    try:
        destination = root / "新版 包" / "tools"
        destination.mkdir(parents=True)
        binary = destination / "plink.exe"
        shutil.copyfile(source, binary)
        check("packaged_plink_hash", hashlib.sha256(binary.read_bytes()).hexdigest() == PLINK_HASH)
        with password_file(root, fixture.password, cleanup) as credential:
            stream = run_plink(binary, fixture, public["fingerprint"], credential, "fixture-stream")
        check("authenticated_exec_exact_unicode_stdout", stream["exit_code"] == 0 and stream["stdout"] == FIRST + FINAL)
        check("separate_stderr", stream["stderr"] == STDERR)
        check("real_first_frame_before_completion", stream["first_stdout_ms"] is not None and
              stream["duration_ms"] - stream["first_stdout_ms"] >= 900)
        check("chinese_and_space_binary_and_password_paths", "中文 空格" in str(binary))
        with password_file(root, secrets.token_urlsafe(32), cleanup) as credential:
            wrong = run_plink(binary, fixture, public["fingerprint"], credential, "fixture-stream")
        check("wrong_password_rejected", wrong["exit_code"] != 0 and not wrong["stdout"])
        with password_file(root, fixture.password, cleanup) as credential:
            rejected = run_plink(binary, fixture, public["fingerprint"], credential, "fixture-reject")
        check("exec_request_rejected", rejected["exit_code"] != 0 and not rejected["stdout"])
        rotated = fixture.rotate()
        with password_file(root, fixture.password, cleanup) as credential:
            changed = run_plink(binary, fixture, public["fingerprint"], credential, "fixture-stream")
        check("changed_host_key_rejected", changed["exit_code"] != 0 and not changed["stdout"])
        with password_file(root, fixture.password, cleanup) as credential:
            trusted = run_plink(binary, fixture, rotated["fingerprint"], credential, "fixture-stream")
        check("explicit_rotated_pin_accepted", trusted["exit_code"] == 0 and trusted["stdout"] == FIRST + FINAL)
        for reason in ("timeout", "cancel"):
            with password_file(root, fixture.password, cleanup) as credential:
                stopped = run_plink(binary, fixture, rotated["fingerprint"], credential,
                                    "fixture-timeout", stop_reason=reason)
            expected_stopped = stopped["timed_out"] if reason == "timeout" else stopped["cancelled"]
            check(reason + "_after_first_frame", expected_stopped and stopped["stdout"] == FIRST and stopped["duration_ms"] < 6500)
        spawn_failed = False
        try:
            with password_file(root, fixture.password, cleanup) as credential:
                run_plink(root / "nonexistent-plink.exe", fixture, rotated["fingerprint"], credential, "fixture-stream")
        except FileNotFoundError:
            spawn_failed = True
        check("spawn_failure_cleanup", spawn_failed and cleanup[-1])
        collision = root / "fixture-existing.tmp"
        collision.write_bytes(b"non-secret-sentinel")
        create_failed = False
        try:
            private_file(collision, b"non-secret-replacement")
        except OSError:
            create_failed = True
        check("credential_create_new_preserves_existing_file", create_failed and collision.read_bytes() == b"non-secret-sentinel")
        collision.unlink()
        deadline = time.monotonic() + 2
        while fixture.metadata("stats")["stats"]["timeout_peer_closed"] < 2 and time.monotonic() < deadline:
            time.sleep(0.05)
        stats = fixture.metadata("stats")["stats"]
        check("server_observed_cancelled_transports", stats["timeout_peer_closed"] == 2)
        check("credentials_removed_all_paths", len(cleanup) == 8 and all(cleanup) and not list(root.glob("*.tmp")))
        check("created_protected_user_only_dacl_verified", len(cleanup) == 8)
        check("server_observed_auth_and_exec_rejections", stats["auth_rejected"] >= 1 and stats["exec_rejected"] == 1)
        report = {"schema": 1, "scope": "real_pinned_plink_loopback_only_not_agent_mcp_or_switch",
                  "plink_sha256": PLINK_HASH, "paramiko_version": paramiko.__version__,
                  "public_server": public, "rotated_fingerprint": rotated["fingerprint"],
                  "checks": checks, "passed": all(item["passed"] for item in checks),
                  "stats": stats, "stream_first_ms": stream["first_stdout_ms"],
                  "stream_duration_ms": stream["duration_ms"]}
        output = Path(args.report).resolve()
        if output.drive.lower() != "c:" or not output.is_relative_to(temp_parent):
            raise ValueError("report_must_be_c_temp")
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        emit({"passed": report["passed"], "checks": len(checks), "report": str(output)})
        return 0 if report["passed"] else 1
    finally:
        fixture.close()
        # 只删除本次自己创建且已解析确认位于 C 临时目录的目录。
        if root.parent == temp_parent and root.name.startswith("RemoteOps-SSH-中文 空格-") and not root.is_symlink():
            shutil.rmtree(root)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="mode", required=True)
    commands.add_parser("serve", help="stdin 传合成密码及控制命令，stdout 仅公共 NDJSON")
    tests = commands.add_parser("test", help="固定官方 Plink 的本机真实 SSH 矩阵")
    tests.add_argument("--plink", required=True)
    tests.add_argument("--report", required=True)
    args = parser.parse_args()
    if paramiko.__version__ != PARAMIKO_VERSION:
        emit({"event": "error", "reason": "paramiko_version_mismatch", "required": PARAMIKO_VERSION})
        return 2
    try:
        return serve() if args.mode == "serve" else matrix(args)
    except Exception as error:
        # 故障类别足以定位夹具，避免第三方异常携带认证输入/命令正文。
        emit({"event": "error", "reason": type(error).__name__})
        return 2


if __name__ == "__main__":
    sys.exit(main())
