#!/usr/bin/env python3
"""在发布机制作固定浏览器候选包；现场 Agent 只下载最终 ZIP。"""

import argparse
import base64
import hashlib
import io
import json
import subprocess
import struct
import zlib
from html.parser import HTMLParser
from pathlib import Path, PurePosixPath
import shutil
import tarfile
import tempfile
import urllib.request
import zipfile

NODE = "22.23.4"
MCP = "0.0.84"
PLAYWRIGHT = "1.65.0-alpha-2026-10-30"
CHROMIUM = "1252"
AGENT_BROWSER = "0.41.2"
CHROME = "157.0.8103.38"


def safe_path(name):
    path = PurePosixPath(name)
    if path.is_absolute() or any(part in ("..", ".", "") for part in name.split("/")) or "\\" in name or ":" in name:
        raise ValueError("包内含不安全路径")
    return path


def fetch(url, downloads, lock, expected_sha256=None, integrity=None):
    if not url.startswith("https://"):
        raise ValueError("发布输入必须使用 HTTPS")
    name = hashlib.sha256(url.encode()).hexdigest()[:16] + "-" + url.rsplit("/", 1)[-1]
    destination = downloads / name
    previous = lock.get(url)
    if destination.exists() and previous and hashlib.sha256(destination.read_bytes()).hexdigest() == previous["sha256"]:
        return destination
    print("获取固定输入:", url, flush=True)
    temporary = destination.with_suffix(destination.suffix + ".partial")
    digest = hashlib.sha256()
    count = 0
    with urllib.request.urlopen(url, timeout=120) as response, temporary.open("wb") as output:
        while chunk := response.read(1024 * 1024):
            count += len(chunk)
            if count > 1024 * 1024 * 1024:
                raise ValueError("发布输入超过 1 GiB")
            output.write(chunk)
            digest.update(chunk)
    sha256 = digest.hexdigest()
    if expected_sha256 and sha256 != expected_sha256:
        raise ValueError("上游 SHA-256 校验失败")
    if previous and previous["sha256"] != sha256:
        raise ValueError("固定 URL 的上游内容发生变化，请人工核查")
    if integrity:
        algorithm, expected = integrity.split("-", 1)
        if algorithm != "sha512" or base64.b64encode(hashlib.sha512(temporary.read_bytes()).digest()).decode() != expected:
            raise ValueError("npm SHA-512 integrity 校验失败")
    temporary.replace(destination)
    lock[url] = {"url": url, "size": count, "sha256": sha256}
    (downloads.parent / "resource-inputs.lock.json").write_text(json.dumps(lock, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return destination


def npm_package(name, version, target, downloads, lock):
    metadata_url = "https://registry.npmjs.org/" + name + "/" + version
    with urllib.request.urlopen(metadata_url, timeout=30) as response:
        metadata = json.load(response)
    if metadata["version"] != version:
        raise ValueError("npm 返回了不同版本")
    archive = fetch(metadata["dist"]["tarball"], downloads, lock, integrity=metadata["dist"]["integrity"])
    target.mkdir(parents=True, exist_ok=True)
    with tarfile.open(archive) as package:
        for member in package:
            name = member.name.removeprefix("package/").rstrip("/")
            if name == "package" or not name:
                continue
            relative = safe_path(name)
            output = target.joinpath(*relative.parts)
            if member.isdir():
                output.mkdir(parents=True, exist_ok=True)
            elif member.isfile():
                output.parent.mkdir(parents=True, exist_ok=True)
                with package.extractfile(member) as source, output.open("wb") as destination:
                    shutil.copyfileobj(source, destination)
            else:
                raise ValueError("npm 输入含链接或特殊文件")
    return metadata


def unzip(archive, target):
    target.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(archive) as package:
        for member in package.infolist():
            name = member.filename.rstrip("/")
            if not name:
                continue
            relative = safe_path(name)
            if (member.external_attr >> 16) & 0o170000 == 0o120000:
                raise ValueError("ZIP 输入含链接")
            output = target.joinpath(*relative.parts)
            if member.is_dir():
                output.mkdir(parents=True, exist_ok=True)
            else:
                output.parent.mkdir(parents=True, exist_ok=True)
                with package.open(member) as source, output.open("wb") as destination:
                    shutil.copyfileobj(source, destination)


class LicenseText(HTMLParser):
    def __init__(self):
        super().__init__()
        self.parts = []
        self.ignored = 0

    def handle_starttag(self, tag, attrs):
        if tag in ("script", "style"):
            self.ignored += 1

    def handle_endtag(self, tag):
        if tag in ("script", "style"):
            self.ignored = max(0, self.ignored - 1)

    def handle_data(self, data):
        if not self.ignored and data.strip():
            self.parts.append(data.strip())


def browser_licenses(package, version, archive_sha256, output, driver=None):
    # 从固定归档中的 DataPack 读取许可证，不启动浏览器或执行安装脚本。
    browser = package / "browser/chrome-win64"
    node = (driver or package) / "node/node.exe"
    licenses = package / "LICENSES"
    licenses.mkdir(exist_ok=True)
    proof = {"browser_version": version, "browser_archive_sha256": archive_sha256, "method": "embedded_datapack_v5", "resources": {}}
    entries = (("credits", "resources.pak", 41071), ("additional-terms", "locales/en-US.pak", 35410))
    for name, relative, resource_id in entries:
        data = (browser / relative).read_bytes()
        if len(data) < 18 or struct.unpack_from("<I", data)[0] != 5:
            raise ValueError("浏览器许可证 DataPack 格式不受支持")
        count = struct.unpack_from("<H", data, 8)[0]
        if 12 + 6 * (count + 1) > len(data):
            raise ValueError("浏览器许可证 DataPack 表越界")
        raw = None
        for index in range(count):
            identifier, offset = struct.unpack_from("<HI", data, 12 + 6 * index)
            end = struct.unpack_from("<I", data, 12 + 6 * (index + 1) + 2)[0]
            if identifier != resource_id:
                continue
            if offset > end or end > len(data):
                raise ValueError("浏览器许可证 DataPack 资源越界")
            compressed = data[offset:end]
            if compressed.startswith(b"\x1f\x8b"):
                raw = zlib.decompress(compressed, 31)
            elif compressed.startswith(b"\x1e\x9b") and len(compressed) >= 8:
                expected = int.from_bytes(compressed[2:8], "little")
                if expected > 16 * 1024 * 1024:
                    raise ValueError("浏览器许可证解压尺寸超限")
                command = [str(node), "-e", "const z=require('zlib'),fs=require('fs');process.stdout.write(z.brotliDecompressSync(fs.readFileSync(0),{maxOutputLength:16777216}));"]
                result = subprocess.run(command, input=compressed[8:], stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30, check=True, creationflags=0x08000000)
                raw = result.stdout
                if len(raw) != expected:
                    raise ValueError("浏览器许可证 Brotli 尺寸不匹配")
            else:
                raw = compressed
            break
        if raw is None or len(raw) > 16 * 1024 * 1024:
            raise ValueError("固定浏览器缺少预期许可证资源")
        parser = LicenseText()
        parser.feed(raw.decode("utf-8"))
        text = ("\n".join(parser.parts) + "\n").encode("utf-8")
        marker = b"Chromium software is made available" if name == "credits" else b"Additional Terms of Service"
        if marker not in text or len(text) < 500:
            raise ValueError("浏览器未包含完整许可证/附加条款")
        prefix = "Browser-" + version + "-" + name
        (licenses / (prefix + ".html")).write_bytes(raw)
        (licenses / (prefix + ".txt")).write_bytes(text)
        proof["resources"][name] = {"source": "browser/chrome-win64/" + relative, "source_sha256": hashlib.sha256(data).hexdigest(), "datapack_version": 5, "resource_id": resource_id, "html_sha256": hashlib.sha256(raw).hexdigest(), "text_sha256": hashlib.sha256(text).hexdigest()}
        print("已提取固定浏览器许可证:", version, name, len(raw), flush=True)
    proof["terms_reference"] = "Additional Terms 引用 Google 主条款 https://policies.google.com/terms；包内保留浏览器自身内嵌文本，未将外链内容宣称为固定版本。"
    (licenses / ("Browser-" + version + "-proof.json")).write_text(json.dumps(proof, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def make_zip(source, destination):
    temporary = destination.with_suffix(destination.suffix + ".partial")
    with zipfile.ZipFile(temporary, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=6) as package:
        for path in sorted(source.rglob("*")):
            if path.is_file():
                info = zipfile.ZipInfo(path.relative_to(source).as_posix(), (2026, 1, 1, 0, 0, 0))
                info.compress_type = zipfile.ZIP_DEFLATED
                info.external_attr = 0o100755 << 16
                with path.open("rb") as reader, package.open(info, "w", force_zip64=True) as writer:
                    shutil.copyfileobj(reader, writer)
    digest = hashlib.sha256(temporary.read_bytes()).hexdigest()
    destination = destination.with_name(destination.stem + "-" + digest[:16] + destination.suffix)
    if destination.exists():
        if hashlib.sha256(destination.read_bytes()).hexdigest() != digest:
            raise ValueError("已存在的固定摘要产物损坏")
        temporary.unlink()
    else:
        temporary.replace(destination)
    return {"url": "packages/" + destination.name, "format": "zip", "size": destination.stat().st_size, "sha256": digest}


def tool(name, version, package, executable, args, browser, interface):
    return {
        "id": name, "version": version,
        "description": "独立浏览器自动化；不接管客户现有登录状态",
        "capabilities": ["browser.navigation", "browser.snapshot", "browser.interaction"],
        "documentation_url": "https://github.com/microsoft/playwright-mcp" if name == "playwright-mcp" else "https://agent-browser.dev",
        "publisher": "本地发布者（部署前替换）", "license": "Apache-2.0 AND MIT AND LicenseRef-Chrome-for-Testing",
        "limitations": ["实验候选；平台组合必须实测后才能标记 passed", "仅 Windows x86_64，Server 2016 的 Chromium 兼容性不受支持"],
        "release_notes": "固定上游版本，包内附依赖锁文件和上游许可证；不在现场执行 npm install",
        "permissions": ["execute_external_code", "browser_read_write", "network"], "requires_desktop": True,
        "platforms": [{"os": "windows", "arch": "x86_64", "os_versions": label, "status": status, "min_windows_build": minimum, "max_windows_build": maximum} for label, status, minimum, maximum in [("Windows 10", "pending", 17763, 19045), ("Windows 11", "pending", 22000, 26200), ("Windows Server 2016", "unsupported", 14393, 14393), ("Windows Server 2019/2022/2025", "pending", 17763, 26100)]],
        "package": package,
        "startup": {"interface": interface, "executable": executable, "args": args, "browser_executable": browser},
        "health_check": {"method": "tools/list" if interface == "stdio_mcp" else "version", "timeout_secs": 60},
        "timeout_secs": 60,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=Path("artifacts/resource-candidates"))
    args = parser.parse_args()
    output = args.output.resolve()
    downloads = output / "inputs"
    downloads.mkdir(parents=True, exist_ok=True)
    lock_path = output / "resource-inputs.lock.json"
    seed_lock = Path(__file__).with_name("resource-inputs.lock.json")
    lock = json.loads((lock_path if lock_path.exists() else seed_lock).read_text("utf-8"))
    site = output / "site/remoteops/v1"
    packages = site / "packages"
    packages.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="remoteops-resource-build-", dir=output) as staging:
        root = Path(staging)
        playwright = root / "playwright"
        playwright.mkdir()
        sums = urllib.request.urlopen(f"https://nodejs.org/dist/v{NODE}/SHASUMS256.txt", timeout=30).read().decode()
        node_file = f"node-v{NODE}-win-x64.zip"
        node_sha = next(line.split()[0] for line in sums.splitlines() if line.split()[-1] == node_file)
        unzip(fetch(f"https://nodejs.org/dist/v{NODE}/{node_file}", downloads, lock, node_sha), playwright)
        node_dir = playwright / f"node-v{NODE}-win-x64"
        node_dir.rename(playwright / "node")
        module_dir = playwright / "tools/node_modules"
        metadata = npm_package("@playwright/mcp", MCP, module_dir / "@playwright/mcp", downloads, lock)
        if metadata["dependencies"] != {"playwright": PLAYWRIGHT, "playwright-core": PLAYWRIGHT}:
            raise ValueError("MCP 固定依赖发生变化")
        npm_package("playwright", PLAYWRIGHT, module_dir / "playwright", downloads, lock)
        npm_package("playwright-core", PLAYWRIGHT, module_dir / "playwright-core", downloads, lock)
        browsers = json.loads((module_dir / "playwright-core/browsers.json").read_text("utf-8"))
        if next(browser["revision"] for browser in browsers["browsers"] if browser["name"] == "chromium") != CHROMIUM:
            raise ValueError("Playwright Chromium revision 与候选不匹配")
        chromium_version = next(browser["browserVersion"] for browser in browsers["browsers"] if browser["name"] == "chromium")
        unzip(fetch(f"https://cdn.playwright.dev/builds/cft/{chromium_version}/win64/chrome-win64.zip", downloads, lock), playwright / "browser")
        browser_licenses(playwright, chromium_version, lock[f"https://cdn.playwright.dev/builds/cft/{chromium_version}/win64/chrome-win64.zip"]["sha256"], output)
        (playwright / "dependency-lock.json").write_text(json.dumps({"node": NODE, "@playwright/mcp": MCP, "playwright": PLAYWRIGHT, "playwright-core": PLAYWRIGHT, "chromium_revision": CHROMIUM, "inputs": {url: value for url, value in lock.items() if "agent-browser" not in url and "storage.googleapis.com" not in url}}, ensure_ascii=False, indent=2), encoding="utf-8")
        playwright_package = make_zip(playwright, packages / f"playwright-mcp-{MCP}-win-x64.zip")
        agent = root / "agent-browser"
        npm_package("agent-browser", AGENT_BROWSER, agent, downloads, lock)
        # 此组合只发布原生 Windows 文件；不保留无关平台可执行文件。
        for path in (agent / "bin").iterdir():
            if path.name != "agent-browser-win32-x64.exe":
                path.unlink()
        unzip(fetch(f"https://storage.googleapis.com/chrome-for-testing-public/{CHROME}/win64/chrome-win64.zip", downloads, lock), agent / "browser")
        browser_licenses(agent, CHROME, lock[f"https://storage.googleapis.com/chrome-for-testing-public/{CHROME}/win64/chrome-win64.zip"]["sha256"], output, driver=playwright)
        (agent / "dependency-lock.json").write_text(json.dumps({"agent-browser": AGENT_BROWSER, "chrome_for_testing": CHROME, "inputs": {url: value for url, value in lock.items() if "agent-browser" in url or "storage.googleapis.com" in url}}, ensure_ascii=False, indent=2), encoding="utf-8")
        agent_package = make_zip(agent, packages / f"agent-browser-{AGENT_BROWSER}-win-x64.zip")
        index = {"schema_version": 1, "tools": [
            tool("playwright-mcp", MCP, playwright_package, "node/node.exe", ["${package}/tools/node_modules/@playwright/mcp/cli.js"], "browser/chrome-win64/chrome.exe", "stdio_mcp"),
            tool("agent-browser", AGENT_BROWSER, agent_package, "bin/agent-browser-win32-x64.exe", [], "browser/chrome-win64/chrome.exe", "agent_browser_cli"),
        ]}
        for item in index["tools"]:
            required = (item["startup"]["executable"], item["startup"]["browser_executable"])
            base = playwright if item["id"] == "playwright-mcp" else agent
            if any(not (base / relative).is_file() for relative in required):
                raise ValueError("包中缺少声明的启动文件或浏览器")
        (site / "index.json").write_text(json.dumps(index, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    lock_path.write_text(json.dumps(lock, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print("已生成固定候选包和待测清单:", site, flush=True)


if __name__ == "__main__":
    main()
