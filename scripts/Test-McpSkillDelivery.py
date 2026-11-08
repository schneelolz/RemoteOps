"""在临时目录运行真实发布脚本；不操作用户安装、凭据或 Y 盘。"""
from __future__ import annotations
import argparse
import hashlib
import json
import re
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
OPTIONS = None


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def checksums(root):
    paths = sorted(p for p in root.rglob("*") if p.is_file() and p.name != "SHA256SUMS.txt")
    (root / "SHA256SUMS.txt").write_text("\n".join(digest(p) + "  " + p.relative_to(root).as_posix() for p in paths) + "\n")


FAULT_HARNESS = r'''
[CmdletBinding()]
param([string]$Publisher, [string]$FixtureOutput, [string]$Mcp, [string]$Skill,
      [string]$FaultMode, [string]$FaultLog)
$ErrorActionPreference = 'Stop'
function Copy-Item {
    [CmdletBinding()]
    param([string]$LiteralPath, [string]$Destination, [switch]$Recurse, [switch]$Force)
    if ($FaultMode -eq 'ready-copy' -and
        $Destination.StartsWith($FixtureOutput + '-ready-', [StringComparison]::OrdinalIgnoreCase)) {
        # Copy part of the real prepared release before simulating disk failure.
        Microsoft.PowerShell.Management\Copy-Item @PSBoundParameters
        [IO.File]::AppendAllText($FaultLog, "ready-copy`n")
        throw 'Injected ready copy failure'
    }
    Microsoft.PowerShell.Management\Copy-Item @PSBoundParameters
}
function Move-Item {
    [CmdletBinding()]
    param([string]$LiteralPath, [string]$Destination, [switch]$Force)
    $backupRoot = $FixtureOutput + '-backups'
    $isBackup = $LiteralPath.Equals($FixtureOutput, [StringComparison]::OrdinalIgnoreCase)
    $isActivation = $LiteralPath.StartsWith($FixtureOutput + '-ready-', [StringComparison]::OrdinalIgnoreCase) -and
        $Destination.Equals($FixtureOutput, [StringComparison]::OrdinalIgnoreCase)
    $isRestore = $LiteralPath.StartsWith($backupRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase) -and
        $Destination.Equals($FixtureOutput, [StringComparison]::OrdinalIgnoreCase)
    if ($FaultMode -eq 'backup-move' -and $isBackup) {
        [IO.File]::AppendAllText($FaultLog, "backup-move`n")
        throw 'Injected backup move failure'
    }
    if ($FaultMode -in @('activation-move', 'activation-and-restore') -and $isActivation) {
        [IO.File]::AppendAllText($FaultLog, "activation-move`n")
        throw 'Injected activation move failure'
    }
    if ($FaultMode -eq 'activation-and-restore' -and $isRestore) {
        [IO.File]::AppendAllText($FaultLog, "restore-move`n")
        throw 'Injected restore move failure'
    }
    Microsoft.PowerShell.Management\Move-Item @PSBoundParameters
}
& $Publisher -OutputRoot $FixtureOutput -McpPackageDirectory $Mcp -SkillPackageDirectory $Skill
'''


class DeliveryTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="remoteops-delivery-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.output = self.root / "RemoteOps"

    def publish(self, output=None, mcp=None, skill=None, skill_only=False):
        command = [OPTIONS.powershell, "-NoProfile", "-File", str(ROOT / "scripts/Publish-WindowsMcpDelivery.ps1"),
                   "-OutputRoot", str(output or self.output),
                   "-SkillPackageDirectory", str(skill or OPTIONS.skill)]
        if skill_only:
            command.append("-SkillOnly")
        else:
            command.extend(["-McpPackageDirectory", str(mcp or OPTIONS.mcp)])
        return subprocess.run(command, capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=120)

    def publish_with_fault(self, mode):
        harness = self.root / "publish-fault.ps1"
        harness.write_text(FAULT_HARNESS, encoding="utf-8-sig")
        self.fault_log = self.root / "fault-events.txt"
        command = [OPTIONS.powershell, "-NoProfile", "-File", str(harness),
                   "-Publisher", str(ROOT / "scripts/Publish-WindowsMcpDelivery.ps1"),
                   "-FixtureOutput", str(self.output), "-Mcp", str(OPTIONS.mcp),
                   "-Skill", str(OPTIONS.skill), "-FaultMode", mode, "-FaultLog", str(self.fault_log)]
        result = subprocess.run(command, capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=120)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(self.fault_log.exists(), "Targeted filesystem fault was not reached")
        return result

    def snapshot(self, directory):
        return {p.relative_to(directory).as_posix(): digest(p) for p in directory.rglob("*") if p.is_file()}

    def prepare_previous_delivery(self):
        self.assert_ok(self.publish())
        # Include an unrelated old release artifact to prove complete preservation.
        (self.output / "previous-release-note.txt").write_text("preserve the complete prior delivery", encoding="utf-8")
        return self.snapshot(self.output)

    def assert_ok(self, result):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_publishes_three_archives_with_independent_components(self):
        self.assert_ok(self.publish())
        manifest = json.loads((self.output / "delivery-manifest.json").read_text(encoding="utf-8-sig"))
        for record in [*manifest["components"].values(), manifest["bundle"]]:
            self.assertEqual(digest(self.output / record["archive"]), record["archive_sha256"])
        mcp = self.output / manifest["components"]["mcp"]["package_directory"]
        skill = self.output / manifest["components"]["skill"]["package_directory"]
        self.assertFalse((mcp / "skills").exists())
        self.assertFalse(list(skill.rglob("*.exe")))

    def test_skill_only_preserves_existing_mcp_and_complete_backup(self):
        self.assert_ok(self.publish())
        old = json.loads((self.output / "delivery-manifest.json").read_text())
        mcp = self.output / old["components"]["mcp"]["package_directory"]
        before = {p.relative_to(mcp).as_posix(): digest(p) for p in mcp.rglob("*") if p.is_file()}
        self.assert_ok(self.publish(skill_only=True))
        current = json.loads((self.output / "delivery-manifest.json").read_text())
        self.assertTrue(current["skill_only"])
        self.assertEqual(before, {p.relative_to(mcp).as_posix(): digest(p) for p in mcp.rglob("*") if p.is_file()})
        backups = list((self.root / "RemoteOps-backups").iterdir())
        self.assertEqual(len(backups), 1)
        self.assertEqual(json.loads((backups[0] / "delivery-manifest.json").read_text()), old)

    def test_bad_skill_does_not_replace_previous_delivery(self):
        self.assert_ok(self.publish())
        original = (self.output / "delivery-manifest.json").read_bytes()
        skill = self.root / "bad-skill"
        shutil.copytree(OPTIONS.skill, skill)
        (skill / "skills/remoteops/SKILL.md").write_text("tampered")
        self.assertNotEqual(self.publish(skill=skill).returncode, 0)
        self.assertEqual((self.output / "delivery-manifest.json").read_bytes(), original)
        self.assertFalse((self.root / "RemoteOps-backups").exists())

    def test_incompatible_skill_is_rejected_before_publication(self):
        skill = self.root / "incompatible"
        shutil.copytree(OPTIONS.skill, skill)
        manifest_path = skill / "manifest.json"
        manifest = json.loads(manifest_path.read_text())
        manifest["minimum_mcp_version"] = "99.0.0"
        manifest_path.write_text(json.dumps(manifest))
        checksums(skill)
        self.assertNotEqual(self.publish(skill=skill).returncode, 0)
        self.assertFalse(self.output.exists())

    def test_same_version_changed_component_is_not_overwritten(self):
        self.assert_ok(self.publish())
        original = (self.output / "delivery-manifest.json").read_bytes()
        skill = self.root / "same-version"
        shutil.copytree(OPTIONS.skill, skill)
        readme = skill / "README.md"
        readme.write_bytes(readme.read_bytes() + b"\nUpdated documentation\n")
        manifest_path = skill / "manifest.json"
        manifest = json.loads(manifest_path.read_text())
        entry = next(x for x in manifest["files"] if x["file"] == "README.md")
        entry.update(bytes=readme.stat().st_size, sha256=digest(readme))
        manifest_path.write_text(json.dumps(manifest))
        checksums(skill)
        self.assertNotEqual(self.publish(skill=skill).returncode, 0)
        self.assertEqual((self.output / "delivery-manifest.json").read_bytes(), original)

    def test_ready_copy_failure_keeps_previous_delivery_untouched(self):
        previous = self.prepare_previous_delivery()
        self.publish_with_fault("ready-copy")
        self.assertEqual(self.fault_log.read_text().splitlines(), ["ready-copy"])
        self.assertEqual(self.snapshot(self.output), previous)
        self.assertFalse((self.root / "RemoteOps-backups").exists())
        ready = list(self.root.glob("RemoteOps-ready-*"))
        self.assertEqual(len(ready), 1)
        self.assertTrue(self.snapshot(ready[0]), "Fault did not leave a partial prepared copy")

    def test_backup_move_failure_keeps_previous_delivery_untouched(self):
        previous = self.prepare_previous_delivery()
        self.publish_with_fault("backup-move")
        self.assertEqual(self.fault_log.read_text().splitlines(), ["backup-move"])
        self.assertEqual(self.snapshot(self.output), previous)
        self.assertEqual(list((self.root / "RemoteOps-backups").iterdir()), [])

    def test_activation_move_failure_restores_complete_previous_delivery(self):
        previous = self.prepare_previous_delivery()
        self.publish_with_fault("activation-move")
        self.assertEqual(self.fault_log.read_text().splitlines(), ["activation-move"])
        self.assertEqual(self.snapshot(self.output), previous)
        self.assertEqual(list((self.root / "RemoteOps-backups").iterdir()), [])

    def test_activation_and_restore_failure_keeps_complete_backup_for_manual_recovery(self):
        previous = self.prepare_previous_delivery()
        result = self.publish_with_fault("activation-and-restore")
        self.assertEqual(self.fault_log.read_text().splitlines(), ["activation-move", "restore-move"])
        self.assertFalse(self.output.exists())
        backups = list((self.root / "RemoteOps-backups").iterdir())
        self.assertEqual(len(backups), 1)
        self.assertEqual(self.snapshot(backups[0]), previous)
        self.assertIn("人工恢复", result.stdout + result.stderr)
        self.assertIn(str(backups[0]), result.stdout + result.stderr)

    def test_missing_or_corrupt_component_checksums_are_rejected(self):
        previous = self.prepare_previous_delivery()
        for component, source in [("mcp", OPTIONS.mcp), ("skill", OPTIONS.skill)]:
            for corruption in ["missing", "damaged"]:
                with self.subTest(component=component, corruption=corruption):
                    package = self.root / (component + "-" + corruption)
                    shutil.copytree(source, package)
                    sums = package / "SHA256SUMS.txt"
                    if corruption == "missing":
                        sums.unlink()
                    else:
                        sums.write_text("0" * 64 + "  manifest.json\n", encoding="utf-8")
                    result = self.publish(**{component: package})
                    self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertEqual(self.snapshot(self.output), previous)
                    self.assertFalse((self.root / "RemoteOps-backups").exists())

    def test_skill_only_downgrade_with_valid_package_is_rejected(self):
        previous = self.prepare_previous_delivery()
        skill = self.root / "older-valid-skill"
        shutil.copytree(OPTIONS.skill, skill)
        version = "0.0.9"
        metadata_path = skill / "skills/remoteops/skill.json"
        metadata = json.loads(metadata_path.read_text(encoding="utf-8-sig"))
        metadata["version"] = version
        metadata_path.write_text(json.dumps(metadata), encoding="utf-8")
        instructions = skill / "skills/remoteops/SKILL.md"
        text = instructions.read_text(encoding="utf-8-sig")
        text, count = re.subn(r'(?m)^(  version:) "[^"]+"$', r'\1 "' + version + '"', text, count=1)
        self.assertEqual(count, 1)
        instructions.write_text(text, encoding="utf-8")
        manifest_path = skill / "manifest.json"
        manifest = json.loads(manifest_path.read_text(encoding="utf-8-sig"))
        manifest["version"] = version
        for entry in manifest["files"]:
            path = skill / entry["file"]
            entry.update(bytes=path.stat().st_size, sha256=digest(path))
        manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
        checksums(skill)
        # The package must pass real component validation, so rejection proves
        # the published-version guard rather than a stale digest or metadata.
        validator = self.root / "validate-old-skill.ps1"
        validator.write_text("param([string]$Library,[string]$Package)\n. $Library\n"
                             "$null = Test-RemoteOpsComponentPackage $Package 'remoteops-skill'\n", encoding="utf-8-sig")
        validation = subprocess.run([OPTIONS.powershell, "-NoProfile", "-File", str(validator),
                                     "-Library", str(ROOT / "scripts/RemoteOps-Package.ps1"), "-Package", str(skill)],
                                    capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=30)
        self.assert_ok(validation)
        result = self.publish(skill=skill, skill_only=True)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("降级", result.stdout + result.stderr)
        self.assertEqual(self.snapshot(self.output), previous)
        self.assertFalse((self.root / "RemoteOps-backups").exists())

    def test_unlisted_extra_file_is_rejected(self):
        skill = self.root / "extra"
        shutil.copytree(OPTIONS.skill, skill)
        (skill / "controller-config.json").write_text("{}")
        self.assertNotEqual(self.publish(skill=skill).returncode, 0)
        self.assertFalse(self.output.exists())

    def test_skill_only_rejects_modified_published_mcp(self):
        self.assert_ok(self.publish())
        original = (self.output / "delivery-manifest.json").read_bytes()
        current = json.loads(original)
        mcp = self.output / current["components"]["mcp"]["package_directory"]
        (mcp / "README.md").write_text("modified")
        self.assertNotEqual(self.publish(skill_only=True).returncode, 0)
        self.assertEqual((self.output / "delivery-manifest.json").read_bytes(), original)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--mcp", type=Path, required=True)
    parser.add_argument("--skill", type=Path, required=True)
    parser.add_argument("--powershell", default="pwsh")
    OPTIONS, remaining = parser.parse_known_args()
    unittest.main(argv=[__file__, *remaining], verbosity=2)
