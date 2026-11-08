#!/usr/bin/env python3
"""在隔离目录验证独立 Skill 的安装、更新、冲突保护与卸载。"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
import zipfile
from unittest import mock

PACKAGE: Path
POWERSHELL: str
DRIVERS: list[str]


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def refresh(package: Path, new_version: str | None = None) -> None:
    manifest_path = package / "manifest.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    if new_version:
        metadata_path = package / "skills/remoteops/skill.json"
        metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
        metadata["version"] = new_version
        metadata_path.write_text(json.dumps(metadata, ensure_ascii=False), encoding="utf-8")
        skill_path = package / "skills/remoteops/SKILL.md"
        skill = re.sub(r'^  version: "[^"]+"', f'  version: "{new_version}"', skill_path.read_text(encoding="utf-8"), flags=re.M)
        skill_path.write_text(skill, encoding="utf-8")
        manifest["version"] = new_version
    for entry in manifest["files"]:
        path = package / entry["file"]
        entry["sha256"] = digest(path)
        entry["bytes"] = path.stat().st_size
    manifest_path.write_text(json.dumps(manifest, ensure_ascii=False), encoding="utf-8")


class SkillTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory(prefix="remoteops-skill-test-")
        self.root = Path(self.tmp.name)
        self.package = self.root / "package"
        shutil.copytree(PACKAGE, self.package)
        self.target = self.root / "host/skills/remoteops"
        self.codex = self.root / "host"
        self.codex.mkdir()
        (self.codex / "config.toml").write_bytes(b"unrelated MCP configuration sentinel\r\n")
        (self.codex / "controller-config.json").write_bytes(b'{"fixture":"credential-reference-only"}')
        self.original = {name: (self.codex / name).read_bytes() for name in ("config.toml", "controller-config.json")}

    def tearDown(self) -> None:
        for name, original in self.original.items():
            self.assertEqual((self.codex / name).read_bytes(), original)
        self.tmp.cleanup()

    def assert_single_discoverable_skill(self, target: Path) -> None:
        self.assertEqual(list(target.parent.rglob("SKILL.md")), [target / "SKILL.md"])
        storage = target.parent.parent / "remoteops-skill-backups"
        self.assertFalse(storage == target.parent or target.parent in storage.parents)

    def run_action(self, driver: str, action: str = "install", *, success: bool = True, replace: bool = False, default_home: Path | None = None) -> subprocess.CompletedProcess:
        env = os.environ.copy()
        env["PYTHONUTF8"] = "1"
        # 各 PowerShell 自行建立模块路径，避免从另一主版本继承不兼容模块。
        env = {key: value for key, value in env.items() if key.upper() != "PSMODULEPATH"}
        if default_home:
            env["USERPROFILE"] = str(default_home)
            env["HOME"] = str(default_home)
            env = {key: value for key, value in env.items() if key.upper() != "CODEX_HOME"}
        if driver == "powershell":
            cmd = [POWERSHELL, "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(self.package / ("Install-RemoteOpsSkill.ps1" if action == "install" else "Uninstall-RemoteOpsSkill.ps1"))]
            if not default_home:
                cmd += ["-SkillDirectory", str(self.target)]
            if replace:
                cmd += ["-ReplaceUnversioned"]
        else:
            cmd = [sys.executable, "-X", "utf8", str(self.package / "manage-remoteops-skill.py"), action]
            if not default_home:
                cmd += ["--skill-directory", str(self.target)]
            if replace:
                cmd += ["--replace-unversioned"]
        result = subprocess.run(cmd, env=env, capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=40)
        if success:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def test_install_and_idempotent_reinstall(self) -> None:
        for driver in DRIVERS:
            with self.subTest(driver=driver):
                self.target = self.root / driver / "skills" / "remoteops"
                self.run_action(driver)
                self.assertEqual((self.target / "SKILL.md").read_bytes(), (self.package / "skills/remoteops/SKILL.md").read_bytes())
                self.assertTrue((self.target / ".remoteops-skill-install.json").exists())
                self.run_action(driver)
                self.assertEqual(list((self.target.parent.parent / "remoteops-skill-backups").glob("backup-*")), [])
                self.assert_single_discoverable_skill(self.target)

    def test_contract_references_are_packaged_and_tracked(self) -> None:
        manifest = json.loads((self.package / "manifest.json").read_text(encoding="utf-8"))
        entries = {entry["file"]: entry for entry in manifest["files"]}
        sums = (self.package / "SHA256SUMS.txt").read_text(encoding="utf-8")
        references = ("tool-center-contract.md", "browser-startup.md", "examples/put-tool-external.json", "examples/put-tool-hosted.json", "examples/record-test.json")
        with zipfile.ZipFile(PACKAGE.parent / (PACKAGE.name + ".zip")) as archive:
            for name in references:
                relative = "skills/remoteops/references/" + name
                path = self.package / relative
                self.assertEqual(entries[relative]["sha256"], digest(path))
                self.assertEqual(entries[relative]["bytes"], path.stat().st_size)
                self.assertIn(digest(path) + "  " + relative, sums)
                self.assertEqual(archive.read(relative), path.read_bytes())
        for driver in DRIVERS:
            with self.subTest(driver=driver):
                self.target = self.root / driver / "skills" / "remoteops"
                self.run_action(driver)
                state = json.loads((self.target / ".remoteops-skill-install.json").read_text(encoding="utf-8"))
                names = {entry["file"] for entry in state["files"]}
                for name in references:
                    relative = "references/" + name
                    self.assertIn(relative, names)
                    self.assertEqual((self.target / relative).read_bytes(), (self.package / "skills/remoteops" / relative).read_bytes())
                self.run_action(driver, "uninstall")
                self.assertFalse(self.target.exists())

    def test_reference_local_modification_blocks_update_and_uninstall(self) -> None:
        for driver in DRIVERS:
            with self.subTest(driver=driver):
                self.target = self.root / driver / "skills" / "remoteops"
                self.run_action(driver)
                reference = self.target / "references/tool-center-contract.md"
                reference.write_bytes(b"user edits to contract")
                self.run_action(driver, success=False)
                self.run_action(driver, "uninstall", success=False)
                self.assertEqual(reference.read_bytes(), b"user edits to contract")

    def test_seven_file_state_upgrades_with_browser_reference_and_backup(self) -> None:
        for driver in DRIVERS:
            with self.subTest(driver=driver):
                self.target = self.root / driver / "skills" / "remoteops"
                self.run_action(driver)
                browser_reference = self.target / "references/browser-startup.md"
                self.assertTrue(browser_reference.is_file())
                browser_reference.unlink()
                metadata_path = self.target / "skill.json"
                metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
                metadata["version"] = "0.1.3"
                metadata_path.write_text(json.dumps(metadata), encoding="utf-8")
                state_path = self.target / ".remoteops-skill-install.json"
                state = json.loads(state_path.read_text(encoding="utf-8"))
                state["version"] = "0.1.3"
                state["files"] = [{"file": entry["file"], "sha256": digest(self.target / entry["file"])} for entry in state["files"] if entry["file"] != "references/browser-startup.md"]
                self.assertEqual(len(state["files"]), 7)
                state_path.write_text(json.dumps(state), encoding="utf-8")
                self.run_action(driver)
                self.assertTrue(browser_reference.is_file())
                self.assertEqual(len(json.loads(state_path.read_text(encoding="utf-8"))["files"]), 8)
                self.assertEqual(len(list((self.target.parent.parent / "remoteops-skill-backups").glob("backup-*"))), 1)

    def test_legacy_three_file_state_upgrades_and_keeps_backup(self) -> None:
        for driver in DRIVERS:
            with self.subTest(driver=driver):
                self.target = self.root / driver / "skills" / "remoteops"
                self.run_action(driver)
                metadata = json.loads((self.target / "skill.json").read_text(encoding="utf-8"))
                metadata["version"] = "0.1.2"
                (self.target / "skill.json").write_text(json.dumps(metadata), encoding="utf-8")
                state_path = self.target / ".remoteops-skill-install.json"
                state = json.loads(state_path.read_text(encoding="utf-8"))
                state["version"] = "0.1.2"
                legacy = []
                for entry in state["files"]:
                    if entry["file"].startswith("references/"):
                        (self.target / entry["file"]).unlink()
                    else:
                        legacy.append({"file": entry["file"], "sha256": digest(self.target / entry["file"])})
                for directory in (self.target / "references/examples", self.target / "references"):
                    directory.rmdir()
                state["files"] = legacy
                state_path.write_text(json.dumps(state), encoding="utf-8")
                self.run_action(driver)
                self.assertTrue((self.target / "references/tool-center-contract.md").exists())
                backups = list((self.target.parent.parent / "remoteops-skill-backups").glob("backup-*"))
                self.assertEqual(len(backups), 1)
                self.assertFalse((backups[0] / "references").exists())
                self.assertEqual(json.loads((backups[0] / "skill.json").read_text(encoding="utf-8"))["version"], "0.1.2")

    def test_unversioned_requires_explicit_migration_and_keeps_backup(self) -> None:
        for driver in DRIVERS:
            with self.subTest(driver=driver):
                self.target = self.root / driver / "skills" / "remoteops"
                self.target.mkdir(parents=True)
                (self.target / "SKILL.md").write_bytes(b"legacy locally edited skill")
                (self.target / "notes").mkdir()
                (self.target / "notes/preserved.txt").write_bytes(b"legacy custom resource")
                self.run_action(driver, success=False)
                self.assertEqual((self.target / "SKILL.md").read_bytes(), b"legacy locally edited skill")
                self.run_action(driver, replace=True)
                backups = list((self.target.parent.parent / "remoteops-skill-backups").glob("backup-*"))
                self.assertEqual(len(backups), 1)
                self.assertEqual((backups[0] / "SKILL.md").read_bytes(), b"legacy locally edited skill")
                self.assertEqual((backups[0] / "notes/preserved.txt").read_bytes(), b"legacy custom resource")
                self.assert_single_discoverable_skill(self.target)

    def test_new_version_upgrades_with_backup_and_refuses_downgrade(self) -> None:
        for driver in DRIVERS:
            with self.subTest(driver=driver):
                self.target = self.root / driver / "skills" / "remoteops"
                refresh(self.package, "0.1.0")
                self.run_action(driver)
                original = {path.relative_to(self.target).as_posix(): path.read_bytes() for path in self.target.rglob("*") if path.is_file()}
                refresh(self.package, "0.2.0")
                self.run_action(driver)
                self.assertEqual(json.loads((self.target / "skill.json").read_text())["version"], "0.2.0")
                backups = list((self.target.parent.parent / "remoteops-skill-backups").glob("backup-*"))
                self.assertEqual(len(backups), 1)
                self.assertEqual({path.relative_to(backups[0]).as_posix(): path.read_bytes() for path in backups[0].rglob("*") if path.is_file()}, original)
                self.assert_single_discoverable_skill(self.target)
                refresh(self.package, "0.1.0")
                self.run_action(driver, success=False)
                self.assertEqual(json.loads((self.target / "skill.json").read_text())["version"], "0.2.0")
                self.target.rename(backups[0].parent / "held-new-version")
                backups[0].rename(self.target)
                self.assertEqual({path.relative_to(self.target).as_posix(): path.read_bytes() for path in self.target.rglob("*") if path.is_file()}, original)
                self.assert_single_discoverable_skill(self.target)

    def test_source_tampering_is_rejected_before_install(self) -> None:
        with (self.package / "skills/remoteops/SKILL.md").open("ab") as stream:
            stream.write(b"tampered")
        for driver in DRIVERS:
            with self.subTest(driver=driver):
                self.run_action(driver, success=False)
                self.assertFalse(self.target.exists())

    def test_same_version_with_different_content_is_rejected(self) -> None:
        for driver in DRIVERS:
            with self.subTest(driver=driver):
                self.target = self.root / driver / "skills" / "remoteops"
                self.run_action(driver)
                original = (self.target / "SKILL.md").read_bytes()
                with (self.package / "skills/remoteops/SKILL.md").open("ab") as stream:
                    stream.write(b"\nchanged guidance\n")
                refresh(self.package)
                self.run_action(driver, success=False)
                self.assertEqual((self.target / "SKILL.md").read_bytes(), original)

    def test_local_modifications_survive_update_and_uninstall(self) -> None:
        for driver in DRIVERS:
            with self.subTest(driver=driver):
                self.target = self.root / driver / "skills" / "remoteops"
                self.run_action(driver)
                (self.target / "SKILL.md").write_bytes(b"user edits")
                self.run_action(driver, success=False)
                self.run_action(driver, "uninstall", success=False)
                self.assertEqual((self.target / "SKILL.md").read_bytes(), b"user edits")

    def test_uninstall_preserves_unmanaged_files(self) -> None:
        for driver in DRIVERS:
            with self.subTest(driver=driver):
                self.target = self.root / driver / "skills" / "remoteops"
                self.run_action(driver)
                (self.target / "local-notes.txt").write_bytes(b"keep me")
                self.run_action(driver, "uninstall")
                self.assertEqual((self.target / "local-notes.txt").read_bytes(), b"keep me")
                self.assertFalse((self.target / "SKILL.md").exists())

    def test_default_destinations_are_updated_together(self) -> None:
        for driver in DRIVERS:
            with self.subTest(driver=driver):
                fake_home = self.root / driver
                fake_home.mkdir()
                self.run_action(driver, default_home=fake_home)
                standard = fake_home / ".agents/skills/remoteops/SKILL.md"
                compat = fake_home / ".codex/skills/remoteops/SKILL.md"
                self.assertEqual(standard.read_bytes(), compat.read_bytes())
                self.run_action(driver, "uninstall", default_home=fake_home)
                self.assertFalse(standard.exists())
                self.assertFalse(compat.exists())

    def test_conflicting_second_destination_does_not_change_first(self) -> None:
        for driver in DRIVERS:
            with self.subTest(driver=driver):
                fake_home = self.root / driver
                old = fake_home / ".codex/skills/remoteops"
                old.mkdir(parents=True)
                (old / "SKILL.md").write_bytes(b"legacy conflicting second destination")
                self.run_action(driver, success=False, default_home=fake_home)
                self.assertFalse((fake_home / ".agents/skills/remoteops").exists())
                self.assertEqual((old / "SKILL.md").read_bytes(), b"legacy conflicting second destination")

    def test_multiple_destinations_keep_backups_outside_both_search_roots(self) -> None:
        for driver in DRIVERS:
            with self.subTest(driver=driver):
                fake_home = self.root / driver
                fake_home.mkdir()
                refresh(self.package, "0.1.0")
                self.run_action(driver, default_home=fake_home)
                refresh(self.package, "0.1.1")
                self.run_action(driver, default_home=fake_home)
                for name in (".agents", ".codex"):
                    target = fake_home / name / "skills/remoteops"
                    self.assert_single_discoverable_skill(target)
                    backups = list((target.parent.parent / "remoteops-skill-backups").glob("backup-*"))
                    self.assertEqual(len(backups), 1)
                    self.assertEqual(json.loads((backups[0] / "skill.json").read_text(encoding="utf-8"))["version"], "0.1.0")

    def test_mid_commit_failure_rolls_back_and_keeps_evidence_outside_search_roots(self) -> None:
        old_bytecode_setting = sys.dont_write_bytecode
        sys.dont_write_bytecode = True
        try:
            spec = importlib.util.spec_from_file_location("isolated_skill_manager", self.package / "manage-remoteops-skill.py")
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
        finally:
            sys.dont_write_bytecode = old_bytecode_setting
        targets = [self.root / name / "skills/remoteops" for name in ("first", "second")]
        module.install(self.package, targets, False)
        originals = [{path.relative_to(target).as_posix(): path.read_bytes() for path in target.rglob("*") if path.is_file()} for target in targets]
        refresh(self.package, "0.2.0")
        original_rename = Path.rename

        def fail_second_commit(path: Path, destination: Path) -> Path:
            if path.name.startswith("stage-") and destination == targets[1]:
                raise OSError("synthetic commit failure")
            return original_rename(path, destination)

        with mock.patch.object(Path, "rename", fail_second_commit):
            with self.assertRaisesRegex(OSError, "synthetic commit failure"):
                module.install(self.package, targets, False)
        for target, original in zip(targets, originals):
            self.assertEqual({path.relative_to(target).as_posix(): path.read_bytes() for path in target.rglob("*") if path.is_file()}, original)
            self.assert_single_discoverable_skill(target)
        evidence = list((targets[0].parent.parent / "remoteops-skill-backups").glob("failed-*"))
        self.assertEqual(len(evidence), 1)
        self.assertEqual(json.loads((evidence[0] / "skill.json").read_text(encoding="utf-8"))["version"], "0.2.0")
        stages = list((targets[1].parent.parent / "remoteops-skill-backups").glob("stage-*"))
        self.assertEqual(len(stages), 1)
        self.assertTrue((stages[0] / "SKILL.md").exists())

    @unittest.skipUnless(os.name == "nt", "Windows junction test")
    def test_linked_backup_storage_is_rejected_before_install(self) -> None:
        outside = self.root / "storage-target"
        outside.mkdir()
        (outside / "sentinel.txt").write_bytes(b"keep storage target")
        for driver in DRIVERS:
            with self.subTest(driver=driver):
                self.target = self.root / driver / "skills/remoteops"
                self.target.parent.parent.mkdir()
                storage = self.target.parent.parent / "remoteops-skill-backups"
                env = {key: value for key, value in os.environ.items() if key.upper() != "PSMODULEPATH"}
                result = subprocess.run(
                    [POWERSHELL, "-NoProfile", "-Command", "New-Item -ItemType Junction -Path '" + str(storage).replace("'", "''") + "' -Target '" + str(outside).replace("'", "''") + "' | Out-Null"],
                    capture_output=True, env=env, timeout=20,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.run_action(driver, success=False)
                self.assertFalse(self.target.exists())
                self.assertEqual(list(outside.iterdir()), [outside / "sentinel.txt"])

    def test_manifest_traversal_is_rejected(self) -> None:
        manifest_path = self.package / "manifest.json"
        data = json.loads(manifest_path.read_text(encoding="utf-8"))
        for invalid_path in ("../outside.txt", "/outside.txt"):
            data["files"][0]["file"] = invalid_path
            manifest_path.write_text(json.dumps(data), encoding="utf-8")
            for driver in DRIVERS:
                with self.subTest(path=invalid_path, driver=driver):
                    self.run_action(driver, success=False)
                    self.assertFalse(self.target.exists())

    def test_source_version_mismatch_is_rejected(self) -> None:
        skill_path = self.package / "skills/remoteops/SKILL.md"
        skill_path.write_text(re.sub(r'^  version: "[^"]+"', '  version: "9.9.9"', skill_path.read_text(encoding="utf-8"), flags=re.M), encoding="utf-8")
        refresh(self.package)
        for driver in DRIVERS:
            with self.subTest(driver=driver):
                self.run_action(driver, success=False)
                self.assertFalse(self.target.exists())

    def test_duplicate_manifest_entries_are_rejected(self) -> None:
        path = self.package / "manifest.json"
        manifest = json.loads(path.read_text(encoding="utf-8"))
        manifest["files"].append(manifest["files"][0])
        path.write_text(json.dumps(manifest), encoding="utf-8")
        for driver in DRIVERS:
            with self.subTest(driver=driver):
                self.run_action(driver, success=False)
                self.assertFalse(self.target.exists())

    def test_unlisted_package_files_are_rejected(self) -> None:
        (self.package / "unexpected.txt").write_bytes(b"unlisted payload")
        for driver in DRIVERS:
            with self.subTest(driver=driver):
                self.run_action(driver, success=False)
                self.assertFalse(self.target.exists())

    def test_compatibility_metadata_mismatch_is_rejected(self) -> None:
        path = self.package / "manifest.json"
        original = json.loads(path.read_text(encoding="utf-8"))
        for key, value in (("minimum_mcp_version", "0.2.0-preview.1"), ("protocol", 18)):
            manifest = dict(original)
            manifest[key] = value
            path.write_text(json.dumps(manifest), encoding="utf-8")
            for driver in DRIVERS:
                with self.subTest(key=key, driver=driver):
                    self.run_action(driver, success=False)
                    self.assertFalse(self.target.exists())

    @unittest.skipUnless(os.name == "nt", "Windows junction test")
    def test_linked_source_parent_is_rejected(self) -> None:
        original = self.package / "skills/remoteops/agents"
        external = self.root / "source-agents"
        original.rename(external)
        env = {key: value for key, value in os.environ.items() if key.upper() != "PSMODULEPATH"}
        result = subprocess.run(
            [POWERSHELL, "-NoProfile", "-Command", "New-Item -ItemType Junction -Path '" + str(original).replace("'", "''") + "' -Target '" + str(external).replace("'", "''") + "' | Out-Null"],
            capture_output=True, env=env, timeout=20,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        for driver in DRIVERS:
            with self.subTest(driver=driver):
                self.run_action(driver, success=False)
                self.assertFalse(self.target.exists())

    @unittest.skipUnless(os.name == "nt", "Windows junction test")
    def test_linked_destination_is_rejected_without_changing_its_target(self) -> None:
        outside = self.root / "outside"
        outside.mkdir()
        (outside / "sentinel.txt").write_bytes(b"never modify linked target")
        parent = self.root / "linked"
        parent.mkdir()
        self.target = parent / "remoteops"
        env = {key: value for key, value in os.environ.items() if key.upper() != "PSMODULEPATH"}
        result = subprocess.run(
            [POWERSHELL, "-NoProfile", "-Command", "New-Item -ItemType Junction -Path '" + str(self.target).replace("'", "''") + "' -Target '" + str(outside).replace("'", "''") + "' | Out-Null"],
            capture_output=True, env=env, timeout=20,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        for driver in DRIVERS:
            with self.subTest(driver=driver):
                self.run_action(driver, success=False, replace=True)
                self.assertEqual((outside / "sentinel.txt").read_bytes(), b"never modify linked target")
                self.assertFalse((outside / "SKILL.md").exists())


def main() -> None:
    global PACKAGE, POWERSHELL, DRIVERS
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", type=Path, required=True)
    parser.add_argument("--powershell", default="powershell.exe")
    parser.add_argument("--python-only", action="store_true")
    args, remaining = parser.parse_known_args()
    PACKAGE = args.package.resolve()
    POWERSHELL = args.powershell
    DRIVERS = ["python"] if args.python_only else ["powershell", "python"]
    unittest.main(argv=[sys.argv[0], *remaining])


if __name__ == "__main__":
    main()
