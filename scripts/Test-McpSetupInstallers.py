#!/usr/bin/env python3
"""Run real installer scripts with an isolated fake setup helper.

No Relay, credential store or user configuration is used. On Linux the macOS
platform check is stubbed; native CI runs retain the actual platform check.
The fake's native executable exercises PowerShell 5.1 stdin/path quoting and
proxies Codex editing to the real MCP binary. Only network/credentials are fake.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import platform
import shutil
import signal
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
VERSION = "0.2.0-preview.13"
DUMMY_GRANT = "installer-test-only-not-a-real-grant-0000000000000000"
SETUP = json.dumps({"version": 1, "grant": DUMMY_GRANT, "fixture_label": "測試"}, ensure_ascii=False)
# The fake deliberately writes only nonsecret outputs. Credential logic belongs
# to the Rust helper's own tests, not another implementation in installers.
HELPER_SOURCE = r'''
use std::{env, fs, io::{self, Read, Write}, path::Path};
fn close_stdin_for_fault_fixture() {
    #[cfg(unix)]
    unsafe {
        extern "C" { fn close(fd: i32) -> i32; }
        assert_eq!(close(0), 0);
    }
    #[cfg(windows)]
    unsafe {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetStdHandle(kind: u32) -> *mut std::ffi::c_void;
            fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
        }
        assert_ne!(CloseHandle(GetStdHandle((-10_i32) as u32)), 0);
    }
}
fn main() {
    let args: Vec<String> = env::args().collect();
    if let Ok(forbidden) = env::var("INSTALLER_TEST_FORBIDDEN_EXECUTABLE") {
        if Path::new(&args[0]) == Path::new(&forbidden) {
            fs::write(env::var("INSTALLER_TEST_FORBIDDEN_MARKER").unwrap(), "unexpected execution").unwrap();
            std::process::exit(88);
        }
    }
    if args.iter().any(|a| a == "--version") {
        if args[0].contains("credential-prompt") {
            println!("remoteops-credential-prompt 0.2.0-preview.5");
        } else { println!("remoteops-controller-mcp @MCP_VERSION@"); }
        return;
    }
    let mut log = fs::OpenOptions::new().create(true).append(true)
        .open(env::var("INSTALLER_TEST_CALLS").unwrap()).unwrap();
    writeln!(log, "{}", args[1..].join("\t")).unwrap();
    if args.iter().any(|a| a == "--validate-codex" || a == "--configure-codex" || a == "--unconfigure-codex" || a == "--inspect-codex") {
        let status = std::process::Command::new(env::var("INSTALLER_TEST_CODEX_HELPER").unwrap())
            .args(&args[1..]).status().unwrap();
        std::process::exit(status.code().unwrap_or(1));
    }
    if args.iter().any(|a| a == "--check-credential" || a == "--remove-credential") { return; }
    assert!(args.iter().any(|a| a == "--setup-stdin"));
    if let Ok(marker) = env::var("INSTALLER_TEST_BROKEN_STDIN_PID") {
        fs::write(marker, std::process::id().to_string()).unwrap();
        close_stdin_for_fault_fixture();
        // Installer cleanup must terminate this child after the write fails.
        loop { std::thread::sleep(std::time::Duration::from_secs(1)); }
    }
    let mut input = String::new();
    io::stdin().read_to_string(&mut input).unwrap();
    let expected = fs::read_to_string(env::var("INSTALLER_TEST_EXPECTED").unwrap()).unwrap();
    if input.trim() != expected.trim() || input.trim() == "invalid" {
        eprintln!("invalid setup input (redacted; utf8_bom={}, expected_bytes={}, actual_bytes={})",
            input.starts_with('\u{feff}'), expected.trim().len(), input.trim().len());
        std::process::exit(2);
    }
    if args.iter().any(|a| a == "--setup-preview") {
        println!("{{\"relay\":\"relay.example.com:7443\",\"server_name\":\"relay.example.com\",\"enrollment_url\":\"https://relay.example.com/controller-enrollment\",\"expires_at\":2000000000,\"uses_private_ca\":false}}");
        // Simulate a file being changed after preview: installer must enroll the
        // original in-memory snapshot rather than reread the supplied file.
        if let Ok(path) = env::var("INSTALLER_TEST_MUTATE_FILE") { fs::write(path, "invalid").unwrap(); }
        return;
    }
    assert!(args.iter().any(|a| a == "--setup-enroll"));
    let get_arg = |name: &str| &args[args.iter().position(|a| a == name).unwrap() + 1];
    let state = get_arg("--setup-state");
    if !Path::new(state).exists() { fs::write(state, "{\"credential_id\":\"11111111-1111-4111-8111-111111111111\"}").unwrap(); }
    let failure = env::var("INSTALLER_TEST_FAILURE").unwrap_or_default();
    if failure == "enroll" { eprintln!("enrollment interrupted (redacted)"); std::process::exit(3); }
    if failure == "self-check" { eprintln!("Relay self-check failed (redacted)"); std::process::exit(4); }
    fs::write(get_arg("--setup-output"), "{\"relay\":\"relay.example.com:7443\",\"server_name\":\"relay.example.com\",\"owner_id\":\"22222222-2222-4222-8222-222222222222\",\"credential_id\":\"11111111-1111-4111-8111-111111111111\",\"helper_marker\":\"preserve-generated-config\"}").unwrap();
    println!("{{\"enrolled\":true,\"relay_verified\":true}}");
}
'''


# macOS plutil -lint is a property-list validator, not a JSON validator. This
# stub intentionally refuses it even for valid JSON, matching the native CI
# failure. Extraction parses all JSON, including null-valued sibling fields.
PLUTIL_SOURCE = """#!/usr/bin/env python3
import json
import sys

if len(sys.argv) < 2 or sys.argv[1] != "-extract":
    sys.exit(1)
try:
    with open(sys.argv[-1], encoding="utf-8") as source:
        data = json.load(source)
    value = data[sys.argv[2]]
    if value is None or isinstance(value, (dict, list)):
        sys.exit(1)
    print(str(value).lower() if isinstance(value, bool) else value)
except (OSError, ValueError, KeyError, TypeError):
    sys.exit(1)
"""


def fixture_process_alive(pid):
    if os.name == "nt":
        import ctypes
        from ctypes import wintypes
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        kernel.OpenProcess.restype = wintypes.HANDLE
        kernel.GetExitCodeProcess.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
        kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        handle = kernel.OpenProcess(0x1000, False, pid)
        if not handle:
            error = ctypes.get_last_error()
            if error == 87:  # ERROR_INVALID_PARAMETER: this PID no longer exists.
                return False
            if error == 5:  # Access denied is not proof of exit.
                return True
            raise OSError(error, "Could not inspect isolated helper process")
        try:
            code = wintypes.DWORD()
            if not kernel.GetExitCodeProcess(handle, ctypes.byref(code)):
                raise OSError("Could not inspect isolated helper process")
            return code.value == 259  # STILL_ACTIVE
        finally:
            kernel.CloseHandle(handle)
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False


class InstallerTests(unittest.TestCase):
    target: str
    powershell: str
    fixture_root: Path
    binary: Path
    real_helper: Path

    @classmethod
    def setUpClass(cls):
        cls.fixture = tempfile.TemporaryDirectory(prefix="remoteops-installer-tests-")
        cls.fixture_root = Path(cls.fixture.name)
        source = cls.fixture_root / "helper.rs"
        source.write_text(HELPER_SOURCE.replace("@MCP_VERSION@", VERSION), encoding="utf-8")
        cls.binary = cls.fixture_root / ("helper.exe" if os.name == "nt" else "helper")
        subprocess.run(["rustc", "--edition", "2021", str(source), "-o", str(cls.binary)], check=True)

    @classmethod
    def tearDownClass(cls):
        cls.fixture.cleanup()

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="installer spaces & Unicode-", dir=self.fixture_root)
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.home = self.root / "home"
        self.codex = self.home / "custom Codex 測試"
        self.codex.mkdir(parents=True)
        self.package = self.root / "package"
        shutil.copytree(ROOT / "deploy/controller-mcp" / self.target, self.package)
        suffix = ".exe" if self.target == "windows" else ""
        for name in ["remoteops-controller-mcp", "remoteops-credential-prompt"]:
            shutil.copy2(self.binary, self.package / (name + suffix))
        self.setup_file = self.root / "private setup.json"
        self.setup_file.write_text(SETUP, encoding="utf-8")
        self.expected = self.root / "expected-input"
        self.expected.write_text(SETUP, encoding="utf-8")
        self.calls = self.root / "calls"
        self.config = self.codex / "config.toml"
        self.original = ('# Keep user choices and unrelated MCP entries\n'
                         'approval_policy = "on-request"\n'
                         'model = "example"\n\n'
                         '[profiles.keep.approval_policy.granular]\n'
                         'mcp_elicitations = false\n\n'
                         '[mcp_servers.other]\ncommand = "other"\n'
                         '# trailing comment belongs to the other server\n')
        self.config.write_bytes(self.original.encode())
        self.env = os.environ.copy()
        # Do not inject PowerShell 7 module paths into Windows PowerShell 5.1.
        # Each selected shell must construct its own built-in module search path.
        for key in list(self.env):
            if key.lower() == "psmodulepath":
                self.env.pop(key)
        self.env.update(HOME=str(self.home), USERPROFILE=str(self.home), CODEX_HOME=str(self.codex),
                        INSTALLER_TEST_CALLS=str(self.calls), INSTALLER_TEST_EXPECTED=str(self.expected),
                        INSTALLER_TEST_CODEX_HELPER=str(self.real_helper))
        # Existing legacy env values must never be read, changed or registered in
        # setup mode. The real user-level store is never touched by our helper.
        self.env["REMOTEOPS_CONTROLLER_TOKEN"] = "legacy-dummy-token-must-remain-untouched"
        self.env["REMOTEOPS_CONTROLLER_OWNER_ID"] = "33333333-3333-4333-8333-333333333333"
        if self.target == "macos":
            commands = self.root / "commands"
            commands.mkdir()
            # Never touch the runner's Keychain, including uninstall's legacy cleanup.
            security = commands / "security"
            security.write_text('#!/bin/sh\n[ "$1" = delete-generic-password ]\n')
            security.chmod(0o755)
            if platform.system() != "Darwin":
                uname = commands / "uname"
                uname.write_text('#!/bin/sh\ncase "$1" in -s) echo Darwin;; -m) echo arm64;; *) exit 1;; esac\n')
                uname.chmod(0o755)
                plutil = commands / "plutil"
                plutil.write_text(PLUTIL_SOURCE)
                plutil.chmod(0o755)
            self.env["PATH"] = str(commands) + os.pathsep + self.env["PATH"]

    def run_installer(self, *, stdin=False, code=False, confirm=True, input_content=None, extra=(), console_encoding=None):
        if self.target == "windows":
            command = [self.powershell, "-NoProfile", "-NonInteractive" if confirm else "-NoLogo",
                       "-ExecutionPolicy", "Bypass", "-File", str(self.package / "Install-RemoteOpsMcp.ps1")]
            command += ["-SetupStdin"] if stdin else ["-SetupFile", str(self.setup_file)]
            if confirm:
                command.append("-ConfirmEnrollment")
            if code:
                # Secure Read-Host needs a real terminal on Windows. This scope
                # mock asserts -AsSecureString, without putting grant in argv.
                self.env["INSTALLER_TEST_SCRIPT"] = str(self.package / "Install-RemoteOpsMcp.ps1")
                wrapper = ("$ErrorActionPreference = 'Stop'; function Read-Host { param($Prompt, [switch]$AsSecureString); "
                           "if (-not $AsSecureString) { throw 'Expected secure setup prompt' }; "
                           "$secure = [Security.SecureString]::new(); "
                           "foreach ($character in ([IO.File]::ReadAllText($env:INSTALLER_TEST_EXPECTED)).ToCharArray()) { $secure.AppendChar($character) }; "
                           "$secure.MakeReadOnly(); return $secure }; "
                           "& $env:INSTALLER_TEST_SCRIPT -SetupCode -ConfirmEnrollment")
                command = [self.powershell, "-NoProfile", "-NonInteractive", "-Command", wrapper]
            if console_encoding is not None:
                self.env["INSTALLER_TEST_SCRIPT"] = str(self.package / "Install-RemoteOpsMcp.ps1")
                self.env["INSTALLER_TEST_SETUP_FILE"] = str(self.setup_file)
                encoding = "[Text.UTF8Encoding]::new($true)" if console_encoding == "utf8-bom" else "[Text.Encoding]::GetEncoding(437)"
                source_args = "-SetupStdin" if stdin else "-SetupFile $env:INSTALLER_TEST_SETUP_FILE"
                wrapper = ("$ErrorActionPreference = 'Stop'; [Console]::InputEncoding = " + encoding + "; "
                           "$before = [Console]::InputEncoding; "
                           "& $env:INSTALLER_TEST_SCRIPT " + source_args + " -ConfirmEnrollment; "
                           "if ([Console]::InputEncoding.CodePage -ne $before.CodePage -or "
                           "[Console]::InputEncoding.GetPreamble().Length -ne $before.GetPreamble().Length) { "
                           "throw 'Installer changed host console encoding' }")
                command = [self.powershell, "-NoProfile", "-NonInteractive", "-Command", wrapper]
        else:
            command = ["bash", str(self.package / "install-remoteops-mcp.sh")]
            command += ["--setup-code"] if code else (["--setup-stdin"] if stdin else ["--setup-file", str(self.setup_file)])
            if confirm:
                command.append("--confirm-enrollment")
        result = subprocess.run(command + list(extra), input=input_content if input_content is not None else (SETUP if stdin else ""),
                                capture_output=True, text=True, encoding="utf-8", errors="replace", env=self.env, timeout=30)
        self.assertNotIn(DUMMY_GRANT, result.stdout + result.stderr)
        if self.calls.exists():
            self.assertNotIn(DUMMY_GRANT, self.calls.read_text())
        for path in self.codex.rglob("*"):
            if path.is_file():
                self.assertNotIn(DUMMY_GRANT.encode(), path.read_bytes(), str(path))
        return result

    def assert_success(self, result):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("https://relay.example.com/controller-enrollment", result.stdout)
        config = self.config.read_text()
        self.assertTrue(config.startswith(self.original), config)
        self.assertEqual(config.count("[mcp_servers.remoteops]"), 1)
        self.assertNotIn("env_vars", config)
        self.assertNotIn("launch-remoteops", config)
        connection = json.loads((self.codex / "remoteops/controller-config.json").read_text())
        self.assertEqual(connection["helper_marker"], "preserve-generated-config")
        self.assertIn("credential_id", connection)
        self.assertTrue((self.codex / "skills/remoteops/SKILL.md").exists())
        calls = self.calls.read_text()
        self.assertLess(calls.index("--validate-codex"), calls.index("--setup-preview"))
        self.assertLess(calls.index("--setup-preview"), calls.index("--setup-enroll"))
        self.assertLess(calls.index("--setup-enroll"), calls.index("--configure-codex"))

    def test_file_install_and_repeat_preserve_config(self):
        self.assert_success(self.run_installer())
        installed_config = self.config.read_bytes()
        self.assert_success(self.run_installer())
        self.assertEqual(self.config.read_bytes(), installed_config)
        backups = list(self.codex.glob("config.toml.remoteops-*.bak"))
        self.assertEqual(len(backups), 1)
        self.assertEqual(backups[0].read_bytes(), self.original.encode())

    def test_fresh_codex_home(self):
        self.original = ""
        self.config.unlink()
        self.codex.rmdir()
        self.assert_success(self.run_installer())
        self.assertNotIn("approval_policy", self.config.read_text())

    def test_stdin_install(self):
        self.assert_success(self.run_installer(stdin=True))

    def test_windows_console_encodings_do_not_change_setup_bytes(self):
        if self.target != "windows":
            self.skipTest("Windows/.NET Framework stdin encoding regression")
        for encoding in ["utf8-bom", "oem"]:
            for stdin in [False, True]:
                with self.subTest(encoding=encoding, stdin=stdin):
                    self.assert_success(self.run_installer(stdin=stdin, console_encoding=encoding))

    def test_windows_broken_stdin_terminates_helper(self):
        if self.target != "windows":
            self.skipTest("Windows process-helper cleanup regression")
        marker = self.root / "fault-helper.pid"
        self.env["INSTALLER_TEST_BROKEN_STDIN_PID"] = str(marker)
        # Exceed pipe buffering so a closed reader reliably interrupts Write.
        self.setup_file.write_text("isolated dummy input " * 65536, encoding="utf-8")
        try:
            result = self.run_installer()
            self.assertNotEqual(result.returncode, 0)
            self.assertTrue(marker.exists(), "Broken-pipe helper did not start")
            self.assertFalse(fixture_process_alive(int(marker.read_text())),
                             "Installer left its failed setup helper running")
            self.assertEqual(self.config.read_bytes(), self.original.encode())
        finally:
            # Test failures must not themselves leak the deliberately hung fixture.
            if marker.exists():
                pid = int(marker.read_text())
                if fixture_process_alive(pid):
                    os.kill(pid, signal.SIGTERM)

    def test_hidden_setup_prompt(self):
        self.assert_success(self.run_installer(code=True, input_content=SETUP + "\n"))

    def run_verifier(self, *, output_encoding=None):
        if self.target == "windows":
            command = [self.powershell, "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File",
                       str(self.package / "Test-RemoteOpsMcp.ps1"), "-SkipNetwork"]
            if output_encoding is not None:
                self.env["INSTALLER_TEST_VERIFIER"] = str(self.package / "Test-RemoteOpsMcp.ps1")
                wrapper = ("$ErrorActionPreference = 'Stop'; "
                           "[Console]::OutputEncoding = [Text.Encoding]::GetEncoding(" + str(output_encoding) + "); "
                           "& $env:INSTALLER_TEST_VERIFIER -SkipNetwork")
                command = [self.powershell, "-NoProfile", "-NonInteractive", "-Command", wrapper]
        else:
            command = ["bash", str(self.package / "test-remoteops-mcp.sh"), "--skip-network"]
        result = subprocess.run(command, capture_output=True, text=True, encoding="utf-8", errors="replace", env=self.env, timeout=30)
        self.assertNotIn(DUMMY_GRANT, result.stdout + result.stderr)
        return result

    def assert_verifier_success(self):
        result = self.run_verifier()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("--check-credential", self.calls.read_text())
        self.assertIn("--inspect-codex", self.calls.read_text())

    def test_installed_verifier_uses_native_credential_helper(self):
        self.assert_success(self.run_installer())
        self.assert_verifier_success()

    def test_windows_verifier_reads_utf8_independently_of_console_encoding(self):
        if self.target != "windows":
            self.skipTest("Windows native UTF-8 inspection decoding regression")
        self.assert_success(self.run_installer())
        for code_page in [437, 1252]:
            with self.subTest(code_page=code_page):
                result = self.run_verifier(output_encoding=code_page)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_verifier_accepts_json_nulls_without_modification(self):
        self.assert_success(self.run_installer())
        connection_path = self.codex / "remoteops/controller-config.json"
        connection = json.loads(connection_path.read_text())
        connection.update(ca_cert=None, tls_fingerprint=None)
        connection_path.write_text(json.dumps(connection, indent=2))
        original = connection_path.read_bytes()
        self.assert_verifier_success()
        self.assertEqual(connection_path.read_bytes(), original)

    def test_verifier_rejects_malformed_connection_json_without_modification(self):
        self.assert_success(self.run_installer())
        connection_path = self.codex / "remoteops/controller-config.json"
        valid = connection_path.read_bytes()
        # An otherwise valid owner_id is not enough: the complete JSON must parse.
        for malformed in [valid + b" trailing-data", valid[:-1], b"{not-json}"]:
            with self.subTest(malformed=malformed):
                connection_path.write_bytes(malformed)
                result = self.run_verifier()
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(connection_path.read_bytes(), malformed)

    def test_verifier_rejects_command_outside_installation(self):
        self.assert_success(self.run_installer())
        outside = self.root / ("remoteops-controller-mcp-" + VERSION + (".exe" if self.target == "windows" else ""))
        shutil.copy2(self.binary, outside)
        marker = self.root / "unexpected-execution"
        self.env["INSTALLER_TEST_FORBIDDEN_EXECUTABLE"] = str(outside)
        self.env["INSTALLER_TEST_FORBIDDEN_MARKER"] = str(marker)
        # Use the real editor, not a textual replacement that assumes TOML's
        # basic-string escaping (Windows paths may be emitted as literal strings).
        subprocess.run([str(self.real_helper), "--configure-codex", str(self.config),
                        "--mcp-command", str(outside),
                        "--mcp-config", str(self.codex / "remoteops/controller-config.json"),
                        "--mcp-mode", "agent-controlled"],
                       capture_output=True, check=True, env=self.env)
        inspection = subprocess.run([str(self.real_helper), "--inspect-codex", str(self.config)],
                                    capture_output=True, text=True, encoding="utf-8", check=True, env=self.env)
        self.assertEqual(json.loads(inspection.stdout)["command"], str(outside),
                         "Outside-installation fixture did not change the actual command")
        result = self.run_verifier()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(marker.exists(), "Verifier executed an out-of-installation command")

    def test_macos_uninstall_removes_credential_before_config(self):
        if self.target != "macos":
            self.skipTest("Windows has no bundled uninstaller")
        self.assert_success(self.run_installer())
        result = subprocess.run(["bash", str(self.package / "uninstall-remoteops-mcp.sh")],
                                capture_output=True, text=True, env=self.env, timeout=30)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("--remove-credential", self.calls.read_text())
        self.assertNotIn("[mcp_servers.remoteops]", self.config.read_text())
        self.assertFalse((self.codex / "remoteops/controller-config.json").exists())
        self.assertFalse((self.codex / "remoteops/setup-state.json").exists())

    def test_changed_file_does_not_change_confirmed_input(self):
        self.env["INSTALLER_TEST_MUTATE_FILE"] = str(self.setup_file)
        self.assert_success(self.run_installer())
        self.assertEqual(self.setup_file.read_text(), "invalid")

    def test_stdin_requires_destination_confirmation(self):
        result = self.run_installer(stdin=True, confirm=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("--setup-enroll", self.calls.read_text())
        self.assertEqual(self.config.read_bytes(), self.original.encode())

    def test_interactive_decline_does_not_enroll(self):
        result = self.run_installer(confirm=False, input_content="no\n")
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("--setup-enroll", self.calls.read_text())
        self.assertEqual(self.config.read_bytes(), self.original.encode())

    def test_invalid_input_fails_before_config_change(self):
        self.setup_file.write_text("invalid")
        self.expected.write_text("invalid")
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("--setup-enroll", self.calls.read_text())
        self.assertEqual(self.config.read_bytes(), self.original.encode())

    def test_failed_enrollment_retains_checkpoint_and_retry_succeeds(self):
        self.env["INSTALLER_TEST_FAILURE"] = "enroll"
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.config.read_bytes(), self.original.encode())
        state = self.codex / "remoteops/setup-state.json"
        checkpoint = state.read_bytes()
        self.env.pop("INSTALLER_TEST_FAILURE")
        self.assert_success(self.run_installer())
        self.assertEqual(state.read_bytes(), checkpoint)

    def test_failed_self_check_does_not_register_or_report_success(self):
        self.env["INSTALLER_TEST_FAILURE"] = "self-check"
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("已安装", result.stdout)
        self.assertEqual(self.config.read_bytes(), self.original.encode())
        self.assertTrue((self.codex / "remoteops/setup-state.json").exists())
        self.assertFalse((self.codex / "remoteops/controller-config.json").exists())

    def test_failed_replacement_preserves_working_connection(self):
        self.assert_success(self.run_installer())
        live_config = self.codex / "remoteops/controller-config.json"
        previous_connection = live_config.read_bytes()
        previous_codex = self.config.read_bytes()
        self.env["INSTALLER_TEST_FAILURE"] = "self-check"
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(live_config.read_bytes(), previous_connection)
        self.assertEqual(self.config.read_bytes(), previous_codex)

    def test_invalid_codex_config_fails_before_install_or_enrollment(self):
        invalid = '[mcp_servers\ncommand = "broken"\n'
        self.config.write_text(invalid)
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.config.read_text(), invalid)
        self.assertFalse((self.codex / "remoteops").exists())
        self.assertNotIn("--setup-enroll", self.calls.read_text())

    def test_fully_quoted_remoteops_header_uses_real_toml_editor(self):
        self.config.write_text(self.original + '["mcp_servers"."remoteops"]\ncommand = "old"\n')
        self.assert_success(self.run_installer())
        self.assertNotIn('command = "old"', self.config.read_text())
        self.assert_verifier_success()

    def test_unrelated_multiline_literal_preserved(self):
        self.original += "notes = " + "'" * 3 + "\n[mcp_servers.remoteops]\nnot a real table\n" + "'" * 3 + "\n"
        self.config.write_text(self.original)
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(self.config.read_text().startswith(self.original))
        self.assertEqual(self.config.read_text().count("[mcp_servers.remoteops]"), 2)
        self.assert_verifier_success()

    def test_replaces_only_remoteops_including_quoted_nested_tables(self):
        old = ('  [mcp_servers."remoteops"] # previous install\ncommand = "old"\n'
               '[mcp_servers.\'remoteops\'.tools.old]\napproval_mode = "prompt"\n')
        tail = '[[unrelated.array]]\nvalue = "preserve"\n'
        self.config.write_text(self.original + old + tail)
        self.assert_success(self.run_installer())
        value = self.config.read_text()
        self.assertIn(tail, value)
        self.assertNotIn('command = "old"', value)
        self.assertNotIn("tools.old", value)

    def test_preserves_mixed_newlines_and_no_final_newline(self):
        # Preserve each unrelated byte; a separator before the new section is OK.
        original = b'# exact comments\r\nmodel = "example"\n[mcp_servers.other]\r\ncommand = "other"'
        self.config.write_bytes(original)
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(self.config.read_bytes().startswith(original))

    def test_manual_and_setup_parameters_cannot_mix(self):
        extra = ["-RelayAddress", "relay.example.com:7443"] if self.target == "windows" else ["--relay", "relay.example.com:7443"]
        result = self.run_installer(extra=extra)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.calls.exists())
        self.assertEqual(self.config.read_bytes(), self.original.encode())

    def test_multiple_sources_cannot_mix(self):
        extra = ["-SetupCode"] if self.target == "windows" else ["--setup-code"]
        result = self.run_installer(extra=extra)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.calls.exists())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--platform", choices=["macos", "windows", "all"], default="all")
    parser.add_argument("--powershell", default="powershell.exe" if os.name == "nt" else "pwsh")
    parser.add_argument("--helper", type=Path, help="Use an already-built real MCP binary for TOML editing; otherwise build it")
    options = parser.parse_args()
    if options.helper is None:
        subprocess.run(["cargo", "build", "--locked", "-p", "remoteops-controller-mcp"], cwd=ROOT, check=True)
        options.helper = ROOT / "target/debug" / ("remoteops-controller-mcp.exe" if os.name == "nt" else "remoteops-controller-mcp")
    options.helper = options.helper.resolve(strict=True)
    version = subprocess.run([str(options.helper), "--version"], capture_output=True, text=True, check=True).stdout
    if VERSION not in version:
        parser.error("Real helper version does not match installer fixtures; build the current MCP first.")
    suite = unittest.TestSuite()
    targets = ["macos", "windows"] if options.platform == "all" else [options.platform]
    for target in targets:
        if target == "macos" and os.name == "nt":
            parser.error("Run macOS installer tests on macOS or Linux, not Windows.")
        test_type = type(target.title() + "InstallerTests", (InstallerTests,), {"target": target, "powershell": options.powershell, "real_helper": options.helper})
        suite.addTests(unittest.defaultTestLoader.loadTestsFromTestCase(test_type))
    return 0 if unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful() else 1


if __name__ == "__main__":
    raise SystemExit(main())
