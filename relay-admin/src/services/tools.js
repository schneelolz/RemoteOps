import { toRaw, nextTick } from "vue";
import { state } from "./store.js";
import {
  apiFetch,
  showToast,
  openModal,
  closeModal,
  navigateTo,
} from "./core.js";
import { isMcpContext, refreshMcpData } from "./mcp.js";
function toolsContext(epoch) {
  return (
    state.isLoggedIn &&
    state.currentTab === "tools" &&
    state.toolCenter.epoch === epoch
  );
}

function invalidateToolsView() {
  state.toolCenter.epoch++;
  state.toolCenter.sequence++;
  state.toolCenter.loading = false;
  state.toolCenter.draft = null;
  state.toolCenter.receipt = null;
  state.toolCenter.error = "";
  state.toolCenter.guideDraft = null;
  state.toolCenter.guideError = "";
  state.toolCenter.returnToDetails = false;
  state.toolCenter.selected = null;
  if (
    state.activeModal?.startsWith("toolCenter") ||
    state.activeModal === "toolDetails"
  ) {
    state.activeModal = null;
    state.modalTargetData = null;
  }
}

async function refreshToolsData() {
  const tc = state.toolCenter;
  if (!state.isLoggedIn || state.currentTab !== "tools" || tc.busy)
    return false;
  const epoch = tc.epoch,
    sequence = ++tc.sequence;
  tc.loading = true;
  tc.error = "";

  try {
    const response = state.demoMode
      ? { data: { revision: 0, enabled: false, tools: [], audit: [] } }
      : await apiFetch("/api/admin/tools");
    if (response.error) throw new Error(response.error.message);
    if (
      !response.data ||
      !Array.isArray(response.data.tools) ||
      !Number.isSafeInteger(response.data.revision)
    )
      throw new Error("工具目录响应无效");
    if (!toolsContext(epoch) || sequence !== tc.sequence) return false;
    tc.snapshot = response.data;
    return true;
  } catch (_) {
    if (toolsContext(epoch) && sequence === tc.sequence)
      tc.error = "工具中心读取失败，请检查管理会话与网络后刷新。";
    return false;
  } finally {
    if (toolsContext(epoch) && sequence === tc.sequence) {
      tc.loading = false;
    }
  }
}

function toolsRequestId() {
  if (typeof crypto.randomUUID === "function") return crypto.randomUUID();
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = (bytes[6] & 15) | 64;
  bytes[8] = (bytes[8] & 63) | 128;
  const hex = Array.from(bytes, (value) =>
    value.toString(16).padStart(2, "0"),
  ).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

async function toolCenterCommand(command) {
  const tc = state.toolCenter;
  if (state.demoMode || !state.isLoggedIn || !tc.snapshot)
    throw new Error("请先读取工具目录");
  const epoch = tc.epoch;
  const request = {
    request_id: toolsRequestId(),
    expected_revision: tc.snapshot.revision,
    command,
  };
  let response;
  try {
    response = await apiFetch("/api/admin/tools", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(request),
    });
  } catch (_) {
    throw new Error(
      `提交结果尚未确认，请刷新核对，勿重复提交。请求标识：${request.request_id}`,
    );
  }
  if (
    response.request_id !== request.request_id ||
    !Number.isSafeInteger(response.revision)
  )
    throw new Error("结果无法确认，请刷新核对");
  if (!toolsContext(epoch)) throw new Error("管理会话已变化，请重新读取目录");
  if (response.error)
    throw new Error(
      response.error.message +
        (response.error.code.includes("revision")
          ? "；请刷新后合并修改。"
          : ""),
    );
  tc.snapshot.revision = response.revision;
  return response.data;
}

async function toolCenterMutation(command, message) {
  const tc = state.toolCenter;
  if (tc.busy || tc.loading || !tc.snapshot || state.demoMode) return;
  const epoch = tc.epoch;
  tc.busy = true;
  tc.error = "";

  try {
    await toolCenterCommand(command);
    if (toolsContext(epoch)) showToast(message, "success");
  } catch (error) {
    if (toolsContext(epoch)) tc.error = error.message;
  } finally {
    tc.busy = false;
    if (toolsContext(epoch)) {
      if (!tc.error) await refreshToolsData();
    } else if (state.isLoggedIn && state.currentTab === "tools") {
      await refreshToolsData();
    }
  }
}

async function saveToolManagementPermission(id, enabled) {
  if (state.demoMode || !state.isLoggedIn || state.currentTab !== "mcp") return;
  const client = state.mcp.clients.find((item) => item.installation_id === id);
  const key = `permission:${id}`,
    epoch = state.mcp.epoch;
  if (!client || client.revoked || state.mcp.revoking.has(key)) return;
  state.mcp.revoking.add(key);

  try {
    const result = await apiFetch(
      `/api/admin/mcp/clients/${encodeURIComponent(id)}/tool-management`,
      {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ enabled }),
      },
    );
    if (result.tool_management !== enabled) throw new Error("授权结果不一致");
    if (isMcpContext(epoch)) {
      client.tool_management = enabled;
      showToast(
        enabled ? "已授予工具管理权限" : "已撤销工具管理权限",
        "success",
      );
    }
  } catch (_) {
    if (isMcpContext(epoch))
      state.mcp.error = "权限变更结果尚未确认，请刷新客户端列表核对。";
  } finally {
    state.mcp.revoking.delete(key);
    finishMcpOperation(epoch);
  }
}

function toolPackages(tool) {
  return tool.versions.flatMap((version) => version.packages);
}

function selectManagedTool(id) {
  const tc = state.toolCenter;
  const tool = tc.snapshot?.tools.find((item) => item.id === id);
  if (!tool || tc.busy) return;
  tc.selected = id;
  tc.detailTab = "guide";
  tc.detailVersion = tool.versions[0]?.version || "";
  tc.returnToDetails = false;
  openModal("toolDetails");
}

const TOOL_GUIDE_TEMPLATE = `# 工具中心
填写本工具中心提供的能力与适用任务。

## 工具选择
- A：填写适用条件、优势与限制。
- B：填写与 A 不同的适用条件。
- C：填写备用场景与前置条件。

## 优先与备用顺序
按任务场景填写首选工具、选择理由和可使用备用工具的条件。
只选择已启用且通过当前平台验证的固定版本。

## 失败处理
启动前失败可核实原因后选择兼容候选。
操作结果未知时先核实实际状态，禁止自动换工具或重复执行。
本说明不能改变现场授权或工具管理权限。`;

function editToolGuide() {
  const tc = state.toolCenter;
  if (tc.busy || tc.loading || !tc.snapshot || state.demoMode) return;
  tc.guideDraft = tc.snapshot.guide_markdown || "";
  tc.guideError = "";

  document.getElementById("tool-guide-input")?.focus();
}

function cancelToolGuide() {
  if (state.toolCenter.busy) return;
  state.toolCenter.guideDraft = null;
  state.toolCenter.guideError = "";

  document.getElementById("tool-guide-edit")?.focus();
}

function useToolGuideTemplate() {
  const tc = state.toolCenter;
  if (tc.busy || tc.guideDraft !== "") return;
  tc.guideDraft = TOOL_GUIDE_TEMPLATE;

  document.getElementById("tool-guide-input")?.focus();
}

function updateToolGuide(value) {
  state.toolCenter.guideDraft = value;
  const button = document.getElementById("tool-guide-template");
  if (button) button.disabled = Boolean(value);
}

async function saveToolGuide(event) {
  event.preventDefault();
  const tc = state.toolCenter;
  if (
    tc.busy ||
    tc.loading ||
    !tc.snapshot ||
    tc.guideDraft === null ||
    state.demoMode
  )
    return;
  const guide = tc.guideDraft,
    epoch = tc.epoch;
  if (new TextEncoder().encode(guide).length > 32768 || guide.includes("\0")) {
    tc.guideError = "全局说明最多 32 KiB，且不能包含 NUL 字符。";

    return;
  }
  tc.busy = true;
  tc.guideError = "";

  try {
    const result = await toolCenterCommand({
      action: "set_guide",
      guide_markdown: guide,
    });
    if (result.guide_markdown !== guide)
      throw new Error("保存结果无法确认，请刷新核对。");
    if (toolsContext(epoch)) {
      tc.snapshot.guide_markdown = guide;
      tc.guideDraft = null;
      showToast("全局说明已保存", "success");
    }
  } catch (error) {
    if (toolsContext(epoch)) tc.guideError = error.message;
  } finally {
    tc.busy = false;
    if (toolsContext(epoch)) {
      document
        .getElementById(tc.guideError ? "tool-guide-error" : "tool-guide-edit")
        ?.focus();
    }
  }
}

/** @type {[string,string][]} */
const TOOL_SECTIONS = [
  ["settings", "全局设置"],
  ["list", "工具列表"],
  ["sync", "节点同步状态"],
];

const TOOL_SYNC_LABELS = {
  synced: "已同步",
  pending: "等待确认",
  update_pending: "目录待更新",
  apply_failed: "应用失败",
  offline: "节点离线",
  upgrade_required: "需要升级 Agent",
};

/** @param {import("../models").SyncNode} node @param {number|undefined} revision */
function toolNodeStatus(node, revision) {
  if (node.supported === false) return "upgrade_required";
  if (node.online === false) return "offline";
  if (Object.hasOwn(TOOL_SYNC_LABELS, node.sync_status))
    return node.sync_status;
  if (node.synced === true) return "synced";
  return node.applied_revision == null ? "pending" : "update_pending";
}

function browserAdapterFacts(startup) {
  if (startup.interface !== "stdio_mcp")
    return {
      registered: "不适用",
      effective:
        startup.interface === "agent_browser_cli"
          ? "agent-browser CLI"
          : "通用 CLI",
      source: "独立 CLI",
      legacy: false,
    };
  const explicit = startup.browser_adapter != null;
  return {
    registered: explicit ? startup.browser_adapter : "未指定",
    effective: explicit
      ? startup.browser_adapter
      : startup.browser_executable
        ? "playwright"
        : "none",
    source: explicit ? "显式配置" : "旧默认规则",
    legacy: !explicit && Boolean(startup.browser_executable),
  };
}

function browserStartupExample(kind) {
  const startup = {
    interface: "stdio_mcp",
    executable: "tool.exe",
    args: [],
    browser_adapter: "none",
    browser_executable: "",
  };
  if (kind === "playwright")
    Object.assign(startup, {
      executable: "node/node.exe",
      args: ["playwright-mcp/cli.js"],
      browser_adapter: "playwright",
      browser_executable: "browser/chrome-win64/chrome.exe",
    });
  if (kind === "agent_browser")
    Object.assign(startup, {
      executable: "bin/agent-browser-win32-x64.exe",
      args: ["mcp", "--tools", "core"],
      browser_adapter: "agent_browser",
      browser_executable: "browser/chrome-win64/chrome.exe",
    });
  if (kind === "agent_browser_cli") {
    Object.assign(startup, {
      interface: "agent_browser_cli",
      executable: "bin/agent-browser-win32-x64.exe",
      browser_executable: "browser/chrome-win64/chrome.exe",
    });
    delete startup.browser_adapter;
  }
  return {
    startup,
    health_check: {
      method: kind === "agent_browser_cli" ? "version" : "tools/list",
      timeout_secs: 120,
    },
  };
}

function packageDefinitionTemplate() {
  const receipt = state.toolCenter.receipt;
  const sha = receipt?.sha256 || "0".repeat(64);
  return [
    {
      version: "1.0.0",
      packages: [
        {
          tool: {
            id: "my-tool",
            version: "1.0.0",
            description: "工具的具体用途",
            capabilities: ["utility"],
            documentation_url: "https://example.com/docs",
            publisher: "工具发布者",
            license: "MIT",
            limitations: [],
            release_notes: "首次登记",
            permissions: ["execute"],
            requires_desktop: false,
            platforms: [
              {
                os: "windows",
                arch: "x86_64",
                os_versions: "实际测试的系统版本",
                status: "pending",
                min_windows_build: 0,
                max_windows_build: 0,
              },
            ],
            package: {
              url:
                receipt?.source.kind === "external"
                  ? receipt.source.url
                  : `relay://packages/${sha}`,
              size: receipt?.size || 1,
              sha256: sha,
              format: "zip",
            },
            startup: {
              interface: "cli",
              executable: "tool.exe",
              args: [],
              browser_executable: "",
              operations: [
                {
                  name: "version",
                  description: "读取版本",
                  input_schema: {
                    type: "object",
                    properties: {},
                    additionalProperties: false,
                  },
                  args: [{ kind: "literal", value: "--version" }],
                  result_format: "text",
                },
              ],
            },
            health_check: { method: "schema", timeout_secs: 30 },
            timeout_secs: 60,
          },
          source: receipt?.source || { kind: "hosted" },
          file_verified: false,
          tests: [],
        },
      ],
    },
  ];
}

/** @param {string} [id] */
function openToolCenterEditor(id) {
  const tc = state.toolCenter;
  if (tc.busy || tc.loading || !tc.snapshot || state.demoMode) return;
  const existing = tc.snapshot.tools.find((tool) => tool.id === id);
  tc.returnToDetails = state.activeModal === "toolDetails";
  tc.draft = existing
    ? clone(existing)
    : {
        id: "",
        name: "",
        purpose: "",
        instructions: "",
        examples: [],
        dependencies: [],
        enabled: false,
        versions: packageDefinitionTemplate(),
      };
  tc.editorTab = "basic";
  tc.draft.versionsText = JSON.stringify(tc.draft.versions, null, 2);
  tc.draft.examplesText = tc.draft.examples.join("\n");
  tc.draft.dependenciesText = tc.draft.dependencies.join("\n");
  tc.error = "";
  openModal("toolCenterEdit", { existing: Boolean(existing) });
}

function openToolPackageImport() {
  if (state.toolCenter.busy || state.demoMode) return;
  state.toolCenter.draft = { source: "hosted", url: "", expected_sha256: "" };
  state.toolCenter.receipt = null;
  state.toolCenter.error = "";
  state.toolCenter.progress = "";
  openModal("toolCenterPackage");
}

function openToolPlatformTest(id, version, sha256) {
  state.toolCenter.returnToDetails = state.activeModal === "toolDetails";
  state.toolCenter.draft = {
    os: "windows",
    arch: "x86_64",
    environment: "",
    desktop: false,
    status: "pending",
    result: "",
    tested_at: new Date().toISOString(),
  };
  state.toolCenter.error = "";
  openModal("toolCenterTest", { id, version, sha256 });
}

async function toolFormRun(work) {
  const tc = state.toolCenter;
  if (tc.busy || state.demoMode) return;
  const epoch = tc.epoch;
  tc.busy = true;
  tc.error = "";

  try {
    await work();
  } catch (error) {
    if (toolsContext(epoch)) tc.error = error.message;
  } finally {
    tc.busy = false;
    if (toolsContext(epoch)) {
      if (tc.error) document.getElementById("tool-form-error")?.focus();
      else {
        await refreshToolsData();
      }
    } else if (state.isLoggedIn && state.currentTab === "tools") {
      await refreshToolsData();
    }
  }
}

function sameStartup(left, right) {
  const normalized = (value) => ({
    interface: value.interface,
    executable: value.executable,
    args: value.args,
    browser_executable: value.browser_executable || "",
    browser_adapter: value.browser_adapter ?? null,
    operations: value.operations || [],
  });
  return JSON.stringify(normalized(left)) === JSON.stringify(normalized(right));
}

async function saveManagedTool(event) {
  event.preventDefault();
  const tc = state.toolCenter;
  let tool;
  try {
    const d = tc.draft;
    tc.editorTab = "basic";
    if (
      !/^[A-Za-z0-9._+-]{1,128}$/.test(d.id.trim()) ||
      !d.name.trim() ||
      !d.purpose.trim() ||
      !d.instructions.trim()
    )
      throw new Error();
    tc.editorTab = "advanced";
    const versions = JSON.parse(d.versionsText);
    if (!Array.isArray(versions) || !versions.length) throw new Error();
    versions.forEach((version) =>
      version.packages.forEach((pkg) => {
        pkg.tool.id = d.id.trim();
        pkg.tool.version = version.version;
      }),
    );
    const existing = tc.snapshot.tools.find(
      (value) => value.id === d.id.trim(),
    );
    for (const version of versions)
      for (const pkg of version.packages) {
        const startup = pkg.tool.startup;
        const previous = existing?.versions
          .find((value) => value.version === version.version)
          ?.packages.find(
            (value) =>
              value.tool.package.sha256 === pkg.tool.package.sha256 &&
              JSON.stringify(
                value.tool.platforms.map((p) => [p.os, p.arch]),
              ) ===
                JSON.stringify(pkg.tool.platforms.map((p) => [p.os, p.arch])),
          );
        if (
          startup.interface === "stdio_mcp" &&
          startup.browser_executable &&
          startup.browser_adapter == null &&
          (!previous || !sameStartup(previous.tool.startup, startup))
        ) {
          tc.error =
            "浏览器 MCP 新增或启动配置变更必须显式设置 browser_adapter 为 playwright 或 agent_browser。";

          return;
        }
      }
    tool = {
      id: d.id.trim(),
      name: d.name.trim(),
      purpose: d.purpose.trim(),
      instructions: d.instructions.trim(),
      examples: d.examplesText
        .split("\n")
        .map((x) => x.trim())
        .filter(Boolean),
      dependencies: d.dependenciesText
        .split("\n")
        .map((x) => x.trim())
        .filter(Boolean),
      enabled: d.enabled,
      versions,
    };
    if (
      !/^[A-Za-z0-9._+-]{1,128}$/.test(tool.id) ||
      !tool.name ||
      !tool.purpose ||
      !tool.instructions
    )
      throw new Error();
  } catch (_) {
    tc.error =
      "请填写完整资料，并检查版本与平台包 JSON；工具标识仅允许字母、数字、点、下划线、加减号。";

    document.getElementById("tool-form-error")?.focus();
    return;
  }
  await toolFormRun(async () => {
    await toolCenterCommand({ action: "put_tool", tool });
    tc.busy = false;
    closeModal();
    showToast("工具资料已保存；启用状态请单独设置", "success");
  });
}

async function saveToolPlatformTest(event) {
  event.preventDefault();
  const tc = state.toolCenter,
    target = state.modalTargetData;
  const record = clone(tc.draft);
  if (
    !record.environment.trim() ||
    !record.result.trim() ||
    Number.isNaN(Date.parse(record.tested_at))
  ) {
    tc.error = "请填写实际环境、验证结果与有效时间。";

    return;
  }
  await toolFormRun(async () => {
    await toolCenterCommand({
      action: "record_test",
      tool_id: target.id,
      version: target.version,
      sha256: target.sha256,
      record,
    });
    tc.busy = false;
    closeModal();
    showToast("测试记录已保存", "success");
  });
}

async function importToolPackage(event) {
  event.preventDefault();
  const tc = state.toolCenter,
    d = clone(tc.draft);
  const file = document.getElementById("tool-zip")?.files[0];
  if (
    d.source === "hosted" &&
    (!file || file.size < 1 || file.size > 1024 * 1024 * 1024)
  ) {
    tc.error = "请选择 1 字节至 1 GiB 的 ZIP 文件。";

    return;
  }
  if (d.source === "external") {
    try {
      const u = new URL(d.url);
      if (u.protocol !== "https:" || u.username || u.password || u.hash)
        throw new Error();
      if (d.expected_sha256 && !/^[0-9a-f]{64}$/.test(d.expected_sha256))
        throw new Error();
    } catch (_) {
      tc.error =
        "请填写不含用户信息与片段的 HTTPS 地址，以及有效的小写 SHA-256。";

      return;
    }
  }
  const epoch = tc.epoch;
  await toolFormRun(async () => {
    if (d.source === "external") {
      tc.progress = "正在临时读取并校验 HTTPS 包…";

      tc.receipt = await toolCenterCommand({
        action: "import_external",
        url: d.url,
        expected_sha256: d.expected_sha256 || null,
      });
    } else {
      tc.progress = "正在计算文件 SHA-256…";

      const hash = new ToolPackageSha256();
      for (let offset = 0; offset < file.size; offset += 256 * 1024) {
        if (!toolsContext(epoch)) throw new Error("管理会话已变化");
        hash.update(
          new Uint8Array(
            await file.slice(offset, offset + 256 * 1024).arrayBuffer(),
          ),
        );
        await new Promise((resolve) => setTimeout(resolve, 0));
      }
      const sha256 = hash.digest();
      const upload = await toolCenterCommand({
        action: "begin_upload",
        size: file.size,
        sha256,
      });
      let offset = 0;
      try {
        while (offset < file.size) {
          if (!toolsContext(epoch)) throw new Error("管理会话已变化");
          const bytes = new Uint8Array(
            await file.slice(offset, offset + 256 * 1024).arrayBuffer(),
          );
          let binary = "";
          for (let i = 0; i < bytes.length; i += 8192)
            binary += String.fromCharCode(...bytes.subarray(i, i + 8192));
          const acknowledgement = await toolCenterCommand({
            action: "upload_chunk",
            upload_id: upload.upload_id,
            offset,
            data_base64: btoa(binary),
          });
          offset += bytes.length;
          if (acknowledgement.offset !== offset)
            throw new Error("上传偏移未确认，已停止传输");
          tc.progress = `正在上传 ${Math.floor((offset / file.size) * 100)}%（每块独立确认）`;
        }
        tc.progress = "正在校验 ZIP 与完整摘要…";

        tc.receipt = await toolCenterCommand({
          action: "finish_upload",
          upload_id: upload.upload_id,
        });
      } catch (error) {
        if (toolsContext(epoch)) {
          try {
            await toolCenterCommand({
              action: "cancel_upload",
              upload_id: upload.upload_id,
            });
          } catch (_) {}
        }
        throw error;
      }
    }
    tc.progress = "文件校验完成；请登记资料与实际测试后再启用。";
  });
}

class ToolPackageSha256 {
  constructor() {
    this.h = new Uint32Array([
      0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c,
      0x1f83d9ab, 0x5be0cd19,
    ]);
    this.buffer = new Uint8Array(64);
    this.used = 0;
    this.length = 0;
    this.w = new Uint32Array(64);
  }
  update(bytes) {
    this.length += bytes.length;
    let i = 0;
    while (i < bytes.length) {
      const n = Math.min(64 - this.used, bytes.length - i);
      this.buffer.set(bytes.subarray(i, i + n), this.used);
      this.used += n;
      i += n;
      if (this.used === 64) {
        this.block();
        this.used = 0;
      }
    }
  }
  block() {
    const k = ToolPackageSha256.k,
      w = this.w,
      b = this.buffer;
    for (let i = 0; i < 16; i++)
      w[i] =
        (b[i * 4] << 24) |
        (b[i * 4 + 1] << 16) |
        (b[i * 4 + 2] << 8) |
        b[i * 4 + 3];
    const r = (v, n) => (v >>> n) | (v << (32 - n));
    for (let i = 16; i < 64; i++) {
      const a = w[i - 15],
        z = w[i - 2];
      w[i] =
        (w[i - 16] +
          (r(a, 7) ^ r(a, 18) ^ (a >>> 3)) +
          w[i - 7] +
          (r(z, 17) ^ r(z, 19) ^ (z >>> 10))) >>>
        0;
    }
    let [a, c, d, e, f, g, h, j] = this.h;
    for (let i = 0; i < 64; i++) {
      const t1 =
          (j +
            (r(f, 6) ^ r(f, 11) ^ r(f, 25)) +
            ((f & g) ^ (~f & h)) +
            k[i] +
            w[i]) >>>
          0,
        t2 =
          ((r(a, 2) ^ r(a, 13) ^ r(a, 22)) + ((a & c) ^ (a & d) ^ (c & d))) >>>
          0;
      j = h;
      h = g;
      g = f;
      f = (e + t1) >>> 0;
      e = d;
      d = c;
      c = a;
      a = (t1 + t2) >>> 0;
    }
    [a, c, d, e, f, g, h, j].forEach((v, i) => {
      this.h[i] = (this.h[i] + v) >>> 0;
    });
  }
  digest() {
    const bytes = this.length;
    this.buffer[this.used++] = 128;
    if (this.used > 56) {
      this.buffer.fill(0, this.used);
      this.block();
      this.used = 0;
    }
    this.buffer.fill(0, this.used, 56);
    const bits = bytes * 8;
    new DataView(this.buffer.buffer).setUint32(
      56,
      Math.floor(bits / 0x100000000),
    );
    new DataView(this.buffer.buffer).setUint32(60, bits >>> 0);
    this.block();
    return Array.from(this.h, (v) => v.toString(16).padStart(8, "0")).join("");
  }
}

ToolPackageSha256.k = new Uint32Array([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1,
  0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
  0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
  0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
  0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
  0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
  0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
  0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
  0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
]);

function clone(value) {
  return structuredClone(toRaw(value));
}

function navigateToolSection(page) {
  void router.push(`/tools/${page}`);
}

function switchToolEditorTab(tab) {
  state.toolCenter.editorTab = tab;
}

function filterTools(value, field) {
  if (["keyword", "filter"].includes(field)) state.toolCenter[field] = value;
}

function filterToolNodes(value, field) {
  if (["nodeKeyword", "nodeFilter"].includes(field)) {
    state.toolCenter[field] = value;
    state.toolCenter.nodePage = 1;
  }
}

export {
  toolsContext,
  invalidateToolsView,
  refreshToolsData,
  toolsRequestId,
  toolCenterCommand,
  toolCenterMutation,
  saveToolManagementPermission,
  toolPackages,
  selectManagedTool,
  TOOL_GUIDE_TEMPLATE,
  editToolGuide,
  cancelToolGuide,
  useToolGuideTemplate,
  updateToolGuide,
  saveToolGuide,
  TOOL_SECTIONS,
  TOOL_SYNC_LABELS,
  toolNodeStatus,
  browserAdapterFacts,
  browserStartupExample,
  packageDefinitionTemplate,
  openToolCenterEditor,
  openToolPackageImport,
  openToolPlatformTest,
  toolFormRun,
  sameStartup,
  saveManagedTool,
  saveToolPlatformTest,
  importToolPackage,
  ToolPackageSha256,
  clone,
  navigateToolSection,
  switchToolEditorTab,
  filterTools,
  filterToolNodes,
};
