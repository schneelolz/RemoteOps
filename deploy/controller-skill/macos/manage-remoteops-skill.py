#!/usr/bin/env python3
"""独立安装或卸载 Skill；不读取 MCP 配置及凭据。"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import sys
import uuid

BASE_FILES = ("SKILL.md", "skill.json", "agents/openai.yaml")
LEGACY_FILES = (*BASE_FILES, "references/tool-center-contract.md", "references/examples/put-tool-external.json", "references/examples/put-tool-hosted.json", "references/examples/record-test.json")
FILES = (*LEGACY_FILES, "references/browser-startup.md")
STATE = ".remoteops-skill-install.json"


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def version(value: str) -> tuple[int, ...]:
    if not isinstance(value, str) or not re.fullmatch(r"\d+\.\d+\.\d+", value):
        raise ValueError("Skill 版本无效")
    return tuple(map(int, value.split(".")))


def no_links(path: Path) -> None:
    for item in (path, *path.parents):
        if item.is_symlink() or (hasattr(item, "is_junction") and item.is_junction()):
            raise ValueError(f"Skill 路径不允许链接：{item}")


def state_files(state: dict) -> tuple[str, ...]:
    names = tuple(entry["file"] for entry in state.get("files", []))
    if len(set(names)) != len(names) or set(names) not in (set(BASE_FILES), set(LEGACY_FILES), set(FILES)):
        raise ValueError("已安装 Skill 状态不完整")
    return names


def installed(path: Path) -> dict:
    no_links(path)
    state = json.loads((path / STATE).read_text(encoding="utf-8"))
    meta = json.loads((path / "skill.json").read_text(encoding="utf-8"))
    if state.get("component") != "remoteops-skill" or state.get("version") != meta.get("version"):
        raise ValueError("已安装 Skill 状态无效")
    state_files(state)
    for entry in state["files"]:
        file = path / entry["file"]
        no_links(file)
        if not file.is_file() or digest(file) != entry["sha256"]:
            raise ValueError("Skill 有本地修改；保留原目录")
    return meta


def package(root: Path) -> dict:
    for path in (root, root / "manifest.json", root / "skills/remoteops/skill.json"):
        no_links(path)
    manifest = json.loads((root / "manifest.json").read_text(encoding="utf-8"))
    meta = json.loads((root / "skills/remoteops/skill.json").read_text(encoding="utf-8"))
    if manifest.get("component") != "remoteops-skill" or manifest.get("version") != meta.get("version") or meta.get("name") != "remoteops" or meta.get("schema_version") != 1 or manifest.get("schema_version") != 1 or manifest.get("minimum_mcp_version") != meta.get("minimum_mcp_version") or manifest.get("protocol") != meta.get("protocol") or manifest.get("contains_credentials") is not False or type(meta.get("protocol")) is not int or not re.fullmatch(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", meta.get("minimum_mcp_version", "")):
        raise ValueError("Skill 包元数据无效")
    version(meta["version"])
    names = []
    for entry in manifest["files"]:
        relative = entry["file"]
        if not re.fullmatch(r"[A-Za-z0-9_./-]+", relative) or Path(relative).is_absolute() or any(part in ("", ".", "..") for part in relative.split("/")):
            raise ValueError("Skill 包含无效路径")
        file = root / relative
        no_links(file)
        if not file.is_file() or file.stat().st_size != entry["bytes"] or digest(file) != entry["sha256"]:
            raise ValueError(f"Skill 包文件校验失败：{relative}")
        names.append(relative)
    if len(set(names)) != len(names):
        raise ValueError("Skill 清单包含重复文件")
    for file in root.rglob("*"):
        no_links(file)
        if file.is_file() and file.relative_to(root).as_posix() not in (*names, "manifest.json", "SHA256SUMS.txt"):
            raise ValueError("Skill 包包含未列入清单的文件")
    for name in FILES:
        if names.count(f"skills/remoteops/{name}") != 1:
            raise ValueError("Skill 清单缺少受管文件")
    text = (root / "skills/remoteops/SKILL.md").read_text(encoding="utf-8")
    if not re.search(r'^  version: "' + re.escape(meta["version"]) + r'"\s*$', text, re.M):
        raise ValueError("SKILL.md 与 skill.json 版本不一致")
    return meta


def install(root: Path, targets: list[Path], replace: bool) -> None:
    meta = package(root)
    plans = []
    for target in targets:
        no_links(target)
        if target.name != "remoteops":
            raise ValueError("Skill 目标目录必须以 remoteops 命名")
        same = False
        if target.exists():
            if not target.is_dir():
                raise ValueError("Skill 目标不是目录")
            if (target / "skill.json").exists():
                old = installed(target)
                if old.get("name") != "remoteops" or version(old["version"]) > version(meta["version"]):
                    raise ValueError("拒绝降级已安装的更新 Skill")
                previous_files = state_files(json.loads((target / STATE).read_text(encoding="utf-8")))
                for file in target.rglob("*"):
                    no_links(file)
                    if file.is_file() and file.relative_to(target).as_posix() not in (*previous_files, STATE):
                        raise ValueError("Skill 含额外文件；保留原目录")
                same = old["version"] == meta["version"]
                if same and (set(previous_files) != set(FILES) or any(digest(target / name) != digest(root / "skills/remoteops" / name) for name in FILES)):
                    raise ValueError("同版 Skill 内容不同；请发布新版本")
            elif not replace:
                raise ValueError("已有未版本化 Skill；确认迁移时使用 --replace-unversioned，原目录将备份")
        storage = target.parent.parent / "remoteops-skill-backups"
        no_links(storage)
        if any(storage == item.parent or item.parent in storage.parents for item in targets):
            raise ValueError("Skill 备份与暂存目录必须位于所有目标 Skill 搜索目录之外")
        plans.append({"target": target, "storage": storage, "same": same, "stage": None, "backup": None, "committed": False})

    def storage_path(plan: dict, kind: str) -> Path:
        path = plan["storage"] / (kind + "-" + uuid.uuid4().hex)
        if path.parent != plan["storage"]:
            raise ValueError("Skill 证据路径超出固定存储目录")
        no_links(path)
        return path

    try:
        for plan in plans:
            if plan["same"]:
                continue
            target = plan["target"]
            target.parent.mkdir(parents=True, exist_ok=True)
            plan["storage"].mkdir(parents=True, exist_ok=True)
            stage = storage_path(plan, "stage")
            plan["stage"] = stage
            (stage / "agents").mkdir(parents=True)
            for name in FILES:
                (stage / name).parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(root / "skills/remoteops" / name, stage / name)
            state = {"component": "remoteops-skill", "version": meta["version"], "files": [{"file": name, "sha256": digest(stage / name)} for name in FILES]}
            (stage / STATE).write_text(json.dumps(state, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        for plan in plans:
            if plan["same"]:
                continue
            target = plan["target"]
            if target.exists():
                backup = storage_path(plan, "backup")
                target.rename(backup)
                plan["backup"] = backup
            plan["stage"].rename(target)
            plan["committed"] = True
    except Exception:
        for plan in plans:
            target = plan["target"]
            if plan["committed"] and target.exists():
                target.rename(storage_path(plan, "failed"))
            if plan["backup"] and not target.exists():
                plan["backup"].rename(target)
        raise
    for plan in plans:
        print(f"RemoteOps Skill {meta['version']}：{plan['target']}")
        if plan["backup"]:
            print(f"原目录备份：{plan['backup']}")
    print(f"兼容要求：MCP >= {meta['minimum_mcp_version']}，协议 {meta['protocol']}；未检查运行中 MCP，未修改 MCP 配置或凭据。")
    print("请新建宿主会话；pi 重开或 /reload。仅更新 Skill 不需要重启 MCP 进程。")


def uninstall(targets: list[Path]) -> None:
    plans = []
    for target in targets:
        no_links(target)
        if target.name != "remoteops":
            raise ValueError("Skill 目标目录必须以 remoteops 命名")
        if target.exists():
            installed(target)
            names = state_files(json.loads((target / STATE).read_text(encoding="utf-8")))
            plans.append((target, names))
    for target, names in plans:
        for name in (*names, STATE):
            (target / name).unlink()
        for directory in (target / "references/examples", target / "references", target / "agents", target):
            if directory.exists() and not any(directory.iterdir()):
                directory.rmdir()
        print(f"RemoteOps Skill 已卸载：{target}")
    print("MCP 程序、配置、凭据和备份均未修改。")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["install", "uninstall"])
    parser.add_argument("--codex-home", type=Path)
    parser.add_argument("--skill-directory", type=Path)
    parser.add_argument("--replace-unversioned", action="store_true")
    args = parser.parse_args()
    if args.codex_home and args.skill_directory:
        parser.error("--codex-home 与 --skill-directory 只能选择一种")
    if args.skill_directory:
        targets = [args.skill_directory.absolute()]
    else:
        codex_home = args.codex_home or Path(os.environ.get("CODEX_HOME", str(Path.home() / ".codex")))
        targets = [codex_home.absolute() / "skills/remoteops"]
        if codex_home.absolute() == Path.home() / ".codex":
            targets.insert(0, Path.home() / ".agents/skills/remoteops")
    if args.action == "install":
        install(Path(__file__).absolute().parent, targets, args.replace_unversioned)
    else:
        uninstall(targets)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, KeyError, TypeError) as error:
        print(f"Skill 操作失败：{error}", file=sys.stderr)
        sys.exit(1)
