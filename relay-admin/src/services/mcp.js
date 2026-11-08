import { state, relayInfo } from "./store.js";
import { apiFetch, showToast, openModal, closeModal } from "./core.js";
function clearMcpSecret() {
  state.mcp.freshSetup = null;
  state.mcp.modalError = null;
  state.mcp.modalEpoch++;
  const secretField = document.getElementById("mcp-setup-code");
  if (secretField) {
    secretField.value = "";
    secretField.textContent = "";
  }
}

function invalidateMcpView(clearMetadata = false) {
  state.mcp.epoch++;
  state.mcp.refreshSequence++;
  state.mcp.policySequence++;
  state.mcp.policyLoading = false;
  state.mcp.loading = false;
  clearMcpSecret();
  if (
    [
      "mcpCreate",
      "mcpFresh",
      "mcpRevoke",
      "mcpRename",
      "mcpLegacyDisable",
    ].includes(state.activeModal)
  ) {
    state.activeModal = null;
    state.modalTargetData = null;
  }
  if (clearMetadata) {
    state.mcp.accessPolicy = null;
    state.mcp.policyLoaded = false;
    state.mcp.policyError = null;
    state.mcp.settings = null;
    state.mcp.settingsDraft = null;
    state.mcp.settingsDirty = false;
    state.mcp.setups = [];
    state.mcp.clients = [];
    state.mcp.loaded = false;
    state.mcp.error = null;
    state.mcp.setupName = "";
    state.mcp.setupExpiry = 24;
  }
}

function isMcpContext(epoch) {
  return (
    state.isLoggedIn && state.currentTab === "mcp" && state.mcp.epoch === epoch
  );
}

function finishMcpOperation(epoch) {
  if (!state.isLoggedIn || state.currentTab !== "mcp") return;
  // Releasing a request lock must also unlock a newer visit to this page. Its data
  // comes from a new read, never from the discarded response or prior dialog.

  if (state.mcp.epoch !== epoch) void refreshMcpData();
}

function mcpSettingsDraft(settings) {
  return Object.fromEntries(
    [
      "relay",
      "server_name",
      "enrollment_url",
      "relay_ca_pem",
      "enrollment_ca_pem",
    ].map((key) => [key, String(settings?.[key] ?? "")]),
  );
}

function updateMcpSetting(key, value) {
  if (
    ![
      "relay",
      "server_name",
      "enrollment_url",
      "relay_ca_pem",
      "enrollment_ca_pem",
    ].includes(key)
  )
    return;
  if (!state.mcp.settingsDraft)
    state.mcp.settingsDraft = mcpSettingsDraft(state.mcp.settings);
  state.mcp.settingsDraft[key] = value;
  state.mcp.settingsDirty = true;
  const createButton = document.getElementById("mcp-new-setup");
  if (createButton) createButton.disabled = true;
}

function validateMcpSettings(draft) {
  const settings = mcpSettingsDraft(draft);
  for (const key of Object.keys(settings)) settings[key] = settings[key].trim();
  const hostPort = settings.relay.match(
    /^(\[[0-9a-fA-F:.]+\]|[^\s/:?#@]+):(\d+)$/,
  );
  if (!hostPort || Number(hostPort[2]) < 1 || Number(hostPort[2]) > 65535) {
    throw new Error(
      "请填写客户端可访问的 Relay TLS host:port（IPv6 地址使用方括号）。",
    );
  }
  let relayUrl;
  try {
    relayUrl = new URL(`https://${settings.relay}`);
  } catch (_) {
    throw new Error("Relay TLS 地址无效。");
  }
  if (["0.0.0.0", "[::]", "*"].includes(relayUrl.hostname)) {
    throw new Error(
      "监听地址不能用于客户端接入，请填写实际可访问的 Relay 主机名或 IP。",
    );
  }
  if (
    !settings.server_name ||
    /[\s/?#@]/.test(settings.server_name) ||
    settings.server_name.includes("://")
  ) {
    throw new Error("请填写 Relay TLS 证书对应的 server_name。");
  }
  let enrollmentUrl;
  try {
    enrollmentUrl = new URL(settings.enrollment_url);
  } catch (_) {
    throw new Error("请填写完整的 HTTPS 注册地址。");
  }
  if (
    enrollmentUrl.protocol !== "https:" ||
    !enrollmentUrl.hostname ||
    enrollmentUrl.username ||
    enrollmentUrl.password ||
    enrollmentUrl.search ||
    enrollmentUrl.hash ||
    ["0.0.0.0", "[::]"].includes(enrollmentUrl.hostname)
  ) {
    throw new Error(
      "注册地址必须为可访问的 HTTPS URL，不得包含用户名、密码、查询参数或片段。",
    );
  }
  for (const key of ["relay_ca_pem", "enrollment_ca_pem"]) {
    const pem = settings[key];
    if (
      pem &&
      (!pem.includes("-----BEGIN CERTIFICATE-----") ||
        !pem.includes("-----END CERTIFICATE-----") ||
        /PRIVATE KEY/i.test(pem))
    ) {
      throw new Error("信任证书只接受公开的 PEM 证书，不得粘贴私钥。");
    }
    settings[key] = pem || null;
  }
  return settings;
}

function parseMcpAccessPolicy(value) {
  if (typeof value?.allow_legacy_token !== "boolean")
    throw new Error("旧 Token 接入策略无效。");
  return { allow_legacy_token: value.allow_legacy_token };
}

async function refreshMcpAccessPolicy() {
  if (
    !state.isLoggedIn ||
    state.currentTab !== "mcp" ||
    state.demoMode ||
    state.mcp.policySaving
  )
    return false;
  const epoch = state.mcp.epoch;
  const sequence = ++state.mcp.policySequence;
  state.mcp.policyLoading = true;
  state.mcp.policyLoaded = false;
  state.mcp.policyError = null;

  try {
    const policy = parseMcpAccessPolicy(
      await apiFetch("/api/admin/mcp/access-policy"),
    );
    if (!isMcpContext(epoch) || sequence !== state.mcp.policySequence)
      return false;
    state.mcp.accessPolicy = policy;
    state.mcp.policyLoaded = true;
    relayInfo.allowLegacyToken = policy.allow_legacy_token;
    return true;
  } catch (_) {
    if (isMcpContext(epoch) && sequence === state.mcp.policySequence) {
      state.mcp.policyError = "旧 Token 接入策略读取失败，请刷新核对后再操作。";
    }
    return false;
  } finally {
    if (isMcpContext(epoch) && sequence === state.mcp.policySequence) {
      state.mcp.policyLoading = false;
    }
  }
}

function requestMcpAccessPolicy(allow) {
  if (
    !state.isLoggedIn ||
    state.currentTab !== "mcp" ||
    state.demoMode ||
    !state.mcp.policyLoaded ||
    state.mcp.policyLoading ||
    state.mcp.policySaving ||
    typeof allow !== "boolean"
  )
    return;

  if (state.mcp.accessPolicy.allow_legacy_token === allow) return;
  if (!allow) openModal("mcpLegacyDisable");
  else void saveMcpAccessPolicy(true);
}

async function saveMcpAccessPolicy(allow) {
  if (
    !state.isLoggedIn ||
    state.currentTab !== "mcp" ||
    state.demoMode ||
    !state.mcp.policyLoaded ||
    state.mcp.policyLoading ||
    state.mcp.policySaving ||
    typeof allow !== "boolean" ||
    (!allow && state.activeModal !== "mcpLegacyDisable")
  )
    return;
  const epoch = state.mcp.epoch;
  const modalEpoch = state.mcp.modalEpoch;
  state.mcp.policySaving = true;
  state.mcp.policyError = null;
  ++state.mcp.policySequence;

  try {
    const policy = parseMcpAccessPolicy(
      await apiFetch("/api/admin/mcp/access-policy", {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ allow_legacy_token: allow }),
      }),
    );
    if (policy.allow_legacy_token !== allow)
      throw new Error("策略保存结果不一致。");
    if (!isMcpContext(epoch)) return;
    state.mcp.accessPolicy = policy;
    state.mcp.policyLoaded = true;
    relayInfo.allowLegacyToken = policy.allow_legacy_token;
    if (state.mcp.modalEpoch === modalEpoch) {
      if (state.activeModal === "mcpLegacyDisable") closeModal();
      showToast(
        allow
          ? "已允许旧 Token 客户端接入"
          : "已关闭旧 Token 接入，旧 MCP 连接将断开",
        "success",
      );
    }
  } catch (_) {
    if (isMcpContext(epoch)) {
      state.mcp.policyLoaded = false;
      state.mcp.policySaving = false;
      const reconciled = await refreshMcpAccessPolicy();
      if (!isMcpContext(epoch)) return;
      state.mcp.policyError = reconciled
        ? "保存结果尚未确认，已重新读取当前策略，请核对后重试。"
        : "保存结果尚未确认，且无法读取当前策略，请刷新核对。";
      if (
        state.mcp.modalEpoch === modalEpoch &&
        state.activeModal === "mcpLegacyDisable"
      )
        closeModal();
    }
  } finally {
    state.mcp.policySaving = false;
    finishMcpOperation(epoch);
  }
}

async function refreshMcpData() {
  if (
    !state.isLoggedIn ||
    state.currentTab !== "mcp" ||
    state.mcp.saving ||
    state.mcp.policySaving
  )
    return false;
  const epoch = state.mcp.epoch;
  const sequence = ++state.mcp.refreshSequence;
  state.mcp.loading = true;
  state.mcp.error = null;

  try {
    const [settings, setups, clients] = state.demoMode
      ? [null, [], []]
      : await Promise.all([
          apiFetch("/api/admin/mcp/settings"),
          apiFetch("/api/admin/mcp/setups"),
          apiFetch("/api/admin/mcp/clients"),
          refreshMcpAccessPolicy(),
        ]);
    if (!isMcpContext(epoch) || sequence !== state.mcp.refreshSequence)
      return false;
    state.mcp.settings = settings;
    if (!state.mcp.settingsDirty)
      state.mcp.settingsDraft = mcpSettingsDraft(settings);
    // Whitelist public metadata. Never retain a secret even if a server adds extra fields.
    state.mcp.setups = (Array.isArray(setups) ? setups : []).map((item) => ({
      grant_id: String(item.grant_id),
      client_name: item.client_name,
      created_at: item.created_at,
      expires_at: item.expires_at,
      state: item.state,
      installation_id: item.installation_id,
    }));
    state.mcp.clients = (Array.isArray(clients) ? clients : []).map((item) => ({
      installation_id: String(item.installation_id),
      client_name: item.client_name,
      created_at: item.created_at,
      last_seen: item.last_seen,
      revoked: Boolean(item.revoked),
      tool_management: item.tool_management === true,
    }));
    state.mcp.loaded = true;
    return true;
  } catch (_) {
    if (isMcpContext(epoch) && sequence === state.mcp.refreshSequence) {
      state.mcp.error =
        "MCP 接入数据加载失败。请检查管理会话与网络，然后刷新重试。";
    }
    return false;
  } finally {
    if (isMcpContext(epoch) && sequence === state.mcp.refreshSequence) {
      state.mcp.loading = false;
    }
  }
}

async function saveMcpSettings(event) {
  event?.preventDefault();
  if (
    !state.isLoggedIn ||
    state.currentTab !== "mcp" ||
    state.demoMode ||
    state.mcp.saving ||
    state.mcp.loading ||
    !state.mcp.loaded
  )
    return;
  let settings;
  try {
    settings = validateMcpSettings(state.mcp.settingsDraft);
  } catch (error) {
    state.mcp.error = error.message;
    return;
  }
  const epoch = state.mcp.epoch;
  state.mcp.saving = true;
  state.mcp.error = null;
  ++state.mcp.refreshSequence;

  try {
    const saved = await apiFetch("/api/admin/mcp/settings", {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(settings),
    });
    if (!isMcpContext(epoch)) return;
    state.mcp.settings = saved;
    state.mcp.settingsDraft = mcpSettingsDraft(saved);
    state.mcp.settingsDirty = false;
    showToast("客户端接入地址与公开信任证书已保存", "success");
  } catch (_) {
    if (isMcpContext(epoch))
      state.mcp.error =
        "保存失败。请检查 HTTPS 地址、TLS 名称及公开 PEM 证书，确认管理会话有效后重试。";
  } finally {
    state.mcp.saving = false;
    finishMcpOperation(epoch);
  }
}

function openMcpSetup() {
  if (
    !state.isLoggedIn ||
    state.currentTab !== "mcp" ||
    state.demoMode ||
    !state.mcp.settings ||
    state.mcp.creating ||
    state.mcp.saving ||
    state.mcp.settingsDirty ||
    state.mcp.loading
  )
    return;
  state.mcp.setupName = "";
  state.mcp.setupExpiry = 24;
  state.mcp.createAttempted = false;
  openModal("mcpCreate");
}

async function createMcpSetup(event) {
  event?.preventDefault();
  if (
    !state.isLoggedIn ||
    state.currentTab !== "mcp" ||
    state.activeModal !== "mcpCreate" ||
    state.demoMode ||
    state.mcp.creating ||
    state.mcp.createAttempted ||
    !state.mcp.settings
  )
    return;
  const expiry = Number(state.mcp.setupExpiry);
  if (![1, 24, 168].includes(expiry)) return;
  let name;
  try {
    name = normalizeMcpClientName(state.mcp.setupName);
  } catch (error) {
    state.mcp.modalError = error.message;
    return;
  }
  const epoch = state.mcp.epoch;
  const modalEpoch = state.mcp.modalEpoch;
  state.mcp.creating = true;
  state.mcp.createAttempted = true;
  state.mcp.modalError = null;

  try {
    const response = await apiFetch("/api/admin/mcp/setups", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        client_name: name || null,
        expires_in_hours: expiry,
      }),
    });
    if (
      !isMcpContext(epoch) ||
      state.mcp.modalEpoch !== modalEpoch ||
      state.activeModal !== "mcpCreate"
    )
      return;
    if (
      !response?.setup ||
      typeof response.setup_code !== "string" ||
      response.setup.version !== 1 ||
      !response.setup.grant_id ||
      !response.setup.grant_secret
    ) {
      throw new Error("Invalid setup response");
    }
    // Both transfer formats come from this same response. Neither is placed in a URL,
    // persistent storage, an event handler, a log, or a later list/detail response.
    state.mcp.freshSetup = {
      setup: response.setup,
      setup_code: response.setup_code,
    };
    state.activeModal = "mcpFresh";

    await refreshMcpData();
  } catch (_) {
    if (
      isMcpContext(epoch) &&
      state.mcp.modalEpoch === modalEpoch &&
      state.activeModal === "mcpCreate"
    ) {
      state.mcp.modalError =
        "未能取得设置代码，结果可能尚未确认。请先刷新设置记录；如已有待使用记录，请撤销后再新建。服务端不会再次返回该代码。";
    }
  } finally {
    state.mcp.creating = false;
    finishMcpOperation(epoch);
  }
}

async function copyMcpSetupCode() {
  if (
    !state.isLoggedIn ||
    state.currentTab !== "mcp" ||
    state.activeModal !== "mcpFresh" ||
    !state.mcp.freshSetup
  )
    return;
  const epoch = state.mcp.epoch;
  const modalEpoch = state.mcp.modalEpoch;
  try {
    await navigator.clipboard.writeText(state.mcp.freshSetup.setup_code);
    if (isMcpContext(epoch) && state.mcp.modalEpoch === modalEpoch)
      showToast("设置代码已复制，请只交给目标客户端", "success");
  } catch (_) {
    if (isMcpContext(epoch) && state.mcp.modalEpoch === modalEpoch)
      showToast("复制失败，请在弹窗中手动选择设置代码", "error");
  }
}

function downloadMcpSetup() {
  if (
    !state.isLoggedIn ||
    state.currentTab !== "mcp" ||
    state.activeModal !== "mcpFresh" ||
    !state.mcp.freshSetup
  )
    return;
  const blob = new Blob(
    [JSON.stringify(state.mcp.freshSetup.setup, null, 2) + "\n"],
    { type: "application/json" },
  );
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = "remoteops.remoteops-setup";
  try {
    document.body.appendChild(link);
    link.click();
  } finally {
    link.remove();
    URL.revokeObjectURL(url);
  }
}

function openMcpRevoke(kind, id) {
  if (
    !state.isLoggedIn ||
    state.currentTab !== "mcp" ||
    state.demoMode ||
    !["setups", "clients"].includes(kind) ||
    state.mcp.revoking.has(`${kind}:${id}`)
  )
    return;
  const item =
    kind === "setups"
      ? state.mcp.setups.find((row) => row.grant_id === id)
      : state.mcp.clients.find((row) => row.installation_id === id);
  if (!item || (kind === "setups" ? item.state === "revoked" : item.revoked))
    return;
  openModal("mcpRevoke", {
    kind,
    id,
    name: item.client_name || "未命名客户端",
  });
}

async function confirmMcpRevoke() {
  if (
    !state.isLoggedIn ||
    state.currentTab !== "mcp" ||
    state.activeModal !== "mcpRevoke" ||
    state.demoMode
  )
    return;
  const data = state.modalTargetData;
  if (!data || !["setups", "clients"].includes(data.kind)) return;
  const key = `${data.kind}:${data.id}`;
  if (state.mcp.revoking.has(key) || data.done) return;
  const epoch = state.mcp.epoch;
  const modalEpoch = state.mcp.modalEpoch;
  state.mcp.revoking.add(key);
  state.mcp.modalError = null;

  try {
    await apiFetch(
      `/api/admin/mcp/${data.kind}/${encodeURIComponent(data.id)}/revoke`,
      { method: "POST" },
    );
    if (
      !isMcpContext(epoch) ||
      state.mcp.modalEpoch !== modalEpoch ||
      state.activeModal !== "mcpRevoke"
    )
      return;
    data.done = true;
    const item =
      data.kind === "setups"
        ? state.mcp.setups.find((row) => row.grant_id === data.id)
        : state.mcp.clients.find((row) => row.installation_id === data.id);
    if (item) {
      if (data.kind === "setups") item.state = "revoked";
      else item.revoked = true;
    }
    closeModal();
    showToast(
      data.kind === "setups" ? "一次性设置已撤销" : "客户端访问凭据已撤销",
      "success",
    );
    await refreshMcpData();
  } catch (_) {
    if (isMcpContext(epoch) && state.mcp.modalEpoch === modalEpoch)
      state.mcp.modalError = "撤销结果尚未确认，请刷新列表核对状态后再重试。";
  } finally {
    state.mcp.revoking.delete(key);
    finishMcpOperation(epoch);
  }
}

function openMcpRename(kind, id) {
  if (
    !state.isLoggedIn ||
    state.currentTab !== "mcp" ||
    state.demoMode ||
    !["setups", "clients"].includes(kind) ||
    state.mcp.renaming.has(`${kind}:${id}`)
  )
    return;
  const item =
    kind === "setups"
      ? state.mcp.setups.find((row) => row.grant_id === id)
      : state.mcp.clients.find((row) => row.installation_id === id);
  if (!item) return;
  openModal("mcpRename", { kind, id, name: item.client_name || "" });
}

function normalizeMcpClientName(value) {
  if (/[\u0000-\u001f\u007f-\u009f]/u.test(value))
    throw new Error("客户端名称不能包含控制字符。");
  const name = value.trim();
  if (new Blob([name]).size > 256)
    throw new Error(
      "客户端名称最多 256 个 UTF-8 字节（中文通常每字 3 字节）。",
    );
  return name || null;
}

async function renameMcpClient(event) {
  event?.preventDefault();
  if (
    !state.isLoggedIn ||
    state.currentTab !== "mcp" ||
    state.activeModal !== "mcpRename" ||
    state.demoMode
  )
    return;
  const data = state.modalTargetData;
  if (!data || !["setups", "clients"].includes(data.kind)) return;
  const key = `${data.kind}:${data.id}`;
  if (state.mcp.renaming.has(key)) return;
  let name;
  try {
    name = normalizeMcpClientName(data.name);
  } catch (error) {
    state.mcp.modalError = error.message;
    return;
  }
  const epoch = state.mcp.epoch;
  const modalEpoch = state.mcp.modalEpoch;
  state.mcp.renaming.add(key);
  state.mcp.modalError = null;
  ++state.mcp.refreshSequence;
  state.mcp.loading = false;

  try {
    await apiFetch(
      `/api/admin/mcp/${data.kind}/${encodeURIComponent(data.id)}/name`,
      {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ client_name: name }),
      },
    );
    if (!isMcpContext(epoch)) return;
    if (
      state.mcp.modalEpoch === modalEpoch &&
      state.activeModal === "mcpRename"
    ) {
      closeModal();
      showToast("客户端名称已保存，关联设置与客户端名称同步更新", "success");
    }
    // 关闭弹窗后仍刷新元数据，保留新弹窗及其输入。
    await refreshMcpData();
  } catch (_) {
    if (
      isMcpContext(epoch) &&
      state.mcp.modalEpoch === modalEpoch &&
      state.activeModal === "mcpRename"
    ) {
      state.mcp.modalError =
        "名称保存结果尚未确认，请关闭后刷新列表核对，再重试。";
    }
  } finally {
    state.mcp.renaming.delete(key);
    finishMcpOperation(epoch);
  }
}

export {
  clearMcpSecret,
  invalidateMcpView,
  isMcpContext,
  finishMcpOperation,
  mcpSettingsDraft,
  updateMcpSetting,
  validateMcpSettings,
  parseMcpAccessPolicy,
  refreshMcpAccessPolicy,
  requestMcpAccessPolicy,
  saveMcpAccessPolicy,
  refreshMcpData,
  saveMcpSettings,
  openMcpSetup,
  createMcpSetup,
  copyMcpSetupCode,
  downloadMcpSetup,
  openMcpRevoke,
  confirmMcpRevoke,
  openMcpRename,
  normalizeMcpClientName,
  renameMcpClient,
};
