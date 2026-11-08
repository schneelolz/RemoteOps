/** 工具中心仅使用已认证管理接口；外链和包内容不进入本地持久存储。 */
state.toolCenter = {
  snapshot: null,
  loading: false,
  busy: false,
  error: '',
  keyword: '',
  filter: 'all',
  selected: null,
  draft: null,
  receipt: null,
  progress: '',
  epoch: 0,
  sequence: 0,
  guideDraft: null,
  guideError: ''
};

function toolsContext(epoch) {
  return (
    state.isLoggedIn &&
    state.currentTab === 'tools' &&
    state.toolCenter.epoch === epoch
  );
}
function invalidateToolsView() {
  state.toolCenter.epoch++;
  state.toolCenter.sequence++;
  state.toolCenter.loading = false;
  state.toolCenter.draft = null;
  state.toolCenter.receipt = null;
  state.toolCenter.error = '';
  state.toolCenter.guideDraft = null;
  state.toolCenter.guideError = '';
  if (state.activeModal?.startsWith('toolCenter')) {
    state.activeModal = null;
    state.modalTargetData = null;
    renderModal();
  }
}

async function refreshToolsData() {
  const tc = state.toolCenter;
  if (!state.isLoggedIn || state.currentTab !== 'tools' || tc.busy)
    return false;
  const epoch = tc.epoch,
    sequence = ++tc.sequence;
  tc.loading = true;
  tc.error = '';
  renderApp();
  try {
    const response = state.demoMode
      ? { data: { revision: 0, enabled: false, tools: [], audit: [] } }
      : await apiFetch('/api/admin/tools');
    if (response.error) throw new Error(response.error.message);
    if (
      !response.data ||
      !Array.isArray(response.data.tools) ||
      !Number.isSafeInteger(response.data.revision)
    )
      throw new Error('工具目录响应无效');
    if (!toolsContext(epoch) || sequence !== tc.sequence) return false;
    tc.snapshot = response.data;
    return true;
  } catch (_) {
    if (toolsContext(epoch) && sequence === tc.sequence)
      tc.error = '工具中心读取失败，请检查管理会话与网络后刷新。';
    return false;
  } finally {
    if (toolsContext(epoch) && sequence === tc.sequence) {
      tc.loading = false;
      renderApp();
    }
  }
}

function toolsRequestId() {
  if (typeof crypto.randomUUID === 'function') return crypto.randomUUID();
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = (bytes[6] & 15) | 64;
  bytes[8] = (bytes[8] & 63) | 128;
  const hex = Array.from(bytes, (value) =>
    value.toString(16).padStart(2, '0')
  ).join('');
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

async function toolCenterCommand(command) {
  const tc = state.toolCenter;
  if (state.demoMode || !state.isLoggedIn || !tc.snapshot)
    throw new Error('请先读取工具目录');
  const epoch = tc.epoch;
  const request = {
    request_id: toolsRequestId(),
    expected_revision: tc.snapshot.revision,
    command
  };
  let response;
  try {
    response = await apiFetch('/api/admin/tools', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(request)
    });
  } catch (_) {
    throw new Error(
      `提交结果尚未确认，请刷新核对，勿重复提交。请求标识：${request.request_id}`
    );
  }
  if (
    response.request_id !== request.request_id ||
    !Number.isSafeInteger(response.revision)
  )
    throw new Error('结果无法确认，请刷新核对');
  if (!toolsContext(epoch)) throw new Error('管理会话已变化，请重新读取目录');
  if (response.error)
    throw new Error(
      response.error.message +
        (response.error.code.includes('revision') ? '；请刷新后合并修改。' : '')
    );
  tc.snapshot.revision = response.revision;
  return response.data;
}

async function toolCenterMutation(command, message) {
  const tc = state.toolCenter;
  if (tc.busy || tc.loading || !tc.snapshot || state.demoMode) return;
  const epoch = tc.epoch;
  tc.busy = true;
  tc.error = '';
  renderApp();
  try {
    await toolCenterCommand(command);
    if (toolsContext(epoch)) showToast(message, 'success');
  } catch (error) {
    if (toolsContext(epoch)) tc.error = error.message;
  } finally {
    tc.busy = false;
    if (toolsContext(epoch)) {
      if (!tc.error) await refreshToolsData();
      else renderApp();
    } else if (state.isLoggedIn && state.currentTab === 'tools') {
      await refreshToolsData();
    }
  }
}

async function saveToolManagementPermission(id, enabled) {
  if (state.demoMode || !state.isLoggedIn || state.currentTab !== 'mcp') return;
  const client = state.mcp.clients.find((item) => item.installation_id === id);
  const key = `permission:${id}`,
    epoch = state.mcp.epoch;
  if (!client || client.revoked || state.mcp.revoking.has(key)) return;
  state.mcp.revoking.add(key);
  renderApp();
  try {
    const result = await apiFetch(
      `/api/admin/mcp/clients/${encodeURIComponent(id)}/tool-management`,
      {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ enabled })
      }
    );
    if (result.tool_management !== enabled) throw new Error('授权结果不一致');
    if (isMcpContext(epoch)) {
      client.tool_management = enabled;
      showToast(
        enabled ? '已授予工具管理权限' : '已撤销工具管理权限',
        'success'
      );
    }
  } catch (_) {
    if (isMcpContext(epoch))
      state.mcp.error = '权限变更结果尚未确认，请刷新客户端列表核对。';
  } finally {
    state.mcp.revoking.delete(key);
    finishMcpOperation(epoch);
  }
}

function toolPackages(tool) {
  return tool.versions.flatMap((version) => version.packages);
}
function toolStatusBadge(passed, yes, no) {
  return `<span class="badge ${passed ? 'badge-online' : 'badge-offline'}">${passed ? yes : no}</span>`;
}
function filterTools(value, kind) {
  const focused = document.activeElement?.id;
  const position = document.activeElement?.selectionStart;
  state.toolCenter[kind] = value;
  renderApp();
  const field = document.getElementById(focused);
  if (field) {
    field.focus();
    if (typeof position === 'number' && field.setSelectionRange)
      field.setSelectionRange(position, position);
  }
}
function selectManagedTool(id) {
  state.toolCenter.selected = id;
  renderApp();
  document.getElementById('tool-details-title')?.focus();
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
  tc.guideDraft = tc.snapshot.guide_markdown || '';
  tc.guideError = '';
  renderApp();
  document.getElementById('tool-guide-input')?.focus();
}

function cancelToolGuide() {
  if (state.toolCenter.busy) return;
  state.toolCenter.guideDraft = null;
  state.toolCenter.guideError = '';
  renderApp();
  document.getElementById('tool-guide-edit')?.focus();
}

function useToolGuideTemplate() {
  const tc = state.toolCenter;
  if (tc.busy || tc.guideDraft !== '') return;
  tc.guideDraft = TOOL_GUIDE_TEMPLATE;
  renderApp();
  document.getElementById('tool-guide-input')?.focus();
}

function updateToolGuide(value) {
  state.toolCenter.guideDraft = value;
  const button = document.getElementById('tool-guide-template');
  if (button) button.disabled = Boolean(value);
}

async function saveToolGuide(event) {
  event.preventDefault();
  const tc = state.toolCenter;
  if (tc.busy || tc.loading || !tc.snapshot || tc.guideDraft === null || state.demoMode) return;
  const guide = tc.guideDraft, epoch = tc.epoch;
  if (new TextEncoder().encode(guide).length > 32768 || guide.includes('\0')) {
    tc.guideError = '全局说明最多 32 KiB，且不能包含 NUL 字符。';
    renderApp();
    return;
  }
  tc.busy = true;
  tc.guideError = '';
  renderApp();
  try {
    const result = await toolCenterCommand({ action: 'set_guide', guide_markdown: guide });
    if (result.guide_markdown !== guide) throw new Error('保存结果无法确认，请刷新核对。');
    if (toolsContext(epoch)) {
      tc.snapshot.guide_markdown = guide;
      tc.guideDraft = null;
      showToast('全局说明已保存', 'success');
    }
  } catch (error) {
    if (toolsContext(epoch)) tc.guideError = error.message;
  } finally {
    tc.busy = false;
    if (toolsContext(epoch)) {
      renderApp();
      document.getElementById(tc.guideError ? 'tool-guide-error' : 'tool-guide-edit')?.focus();
    }
  }
}

function renderToolGuide(disabled) {
  const tc = state.toolCenter;
  return `<section class="tools-guide" aria-labelledby="tool-guide-title"><div class="tools-guide-heading"><h2 class="section-title" id="tool-guide-title">全局介绍与选择指南</h2>${tc.guideDraft === null ? `<button id="tool-guide-edit" class="btn btn-secondary btn-sm" ${disabled ? 'disabled' : ''} onclick="editToolGuide()">编辑说明</button>` : ''}</div>${tc.guideDraft === null
    ? `<pre class="tools-guide-text">${escapeHtml(tc.snapshot?.guide_markdown || '尚未填写全局说明。')}</pre>`
    : `<form onsubmit="saveToolGuide(event)"><label class="input-label" for="tool-guide-input">全局说明</label><textarea id="tool-guide-input" class="input tools-guide-input" rows="12" ${tc.busy ? 'disabled' : ''} oninput="updateToolGuide(this.value)">${escapeHtml(tc.guideDraft)}</textarea>${tc.guideError ? `<p class="mcp-error" id="tool-guide-error" role="alert" tabindex="-1">${escapeHtml(tc.guideError)}</p>` : ''}<div class="tools-guide-actions"><button id="tool-guide-template" type="button" class="btn btn-secondary" ${tc.busy || tc.guideDraft ? 'disabled' : ''} onclick="useToolGuideTemplate()">填入模板</button><button type="button" class="btn btn-secondary" ${tc.busy ? 'disabled' : ''} onclick="cancelToolGuide()">取消</button><button type="submit" class="btn btn-primary" ${tc.busy || state.demoMode ? 'disabled' : ''}>${tc.busy ? '正在保存…' : '保存说明'}</button></div></form>`}</section>`;
}

function renderToolsView() {
  const tc = state.toolCenter,
    snapshot = tc.snapshot;
  const disabled = tc.busy || tc.loading || !snapshot || state.demoMode;
  const keyword = tc.keyword.trim().toLocaleLowerCase();
  const tools = (snapshot?.tools || []).filter(
    (tool) =>
      (!keyword ||
        `${tool.name} ${tool.id} ${tool.purpose}`
          .toLocaleLowerCase()
          .includes(keyword)) &&
      (tc.filter === 'all' ||
        (tc.filter === 'enabled' ? tool.enabled : !tool.enabled))
  );
  const selected = snapshot?.tools.find((tool) => tool.id === tc.selected);
  return `<div class="content-container tool-center">
    <div class="page-header"><div><h1 class="page-title">工具中心</h1><p class="page-desc">统一维护工具、版本包与使用说明，让 Agent 自动发现可用工具。</p></div><button class="btn btn-primary" ${disabled ? 'disabled' : ''} onclick="openToolCenterEditor()">新增工具</button></div>
    <section class="card tools-policy" aria-labelledby="tools-policy-title"><div><h2 class="section-title" id="tools-policy-title">工具分发与运行</h2><p class="section-desc">${snapshot?.enabled ? '已开启，Agent 可按现有授权运行已启用的工具。' : '当前关闭，可继续上传包、维护资料和登记测试。开启后才允许分发、启动与调用。'}</p><p class="mcp-note">目录修订 ${snapshot?.revision ?? '尚未读取'}。文件校验、平台测试和启用状态分别管理。未验证不代表不兼容；测试记录只覆盖实际系统与接口。</p></div><label class="tool-permission"><input type="checkbox" role="switch" aria-label="工具中心全局开关" ${snapshot?.enabled ? 'checked' : ''} ${disabled ? 'disabled' : ''} onchange="toolCenterMutation({action:'set_policy',enabled:this.checked},this.checked?'工具中心已开启':'工具中心已关闭')" /> ${snapshot?.enabled ? '已开启' : '已关闭'}</label></section>
    ${state.demoMode ? '<p class="mcp-note" role="status">演示模式仅展示界面，不保存工具或变更开关。</p>' : ''}
    ${tc.error ? `<div class="mcp-error" role="alert">${escapeHtml(tc.error)}</div>` : ''}
    ${renderToolGuide(disabled)}
    <div class="tools-toolbar"><div class="input-group"><label for="tools-search" class="input-label">搜索工具</label><input id="tools-search" class="input" type="search" value="${escapeHtml(tc.keyword)}" placeholder="名称、标识或用途" oninput="filterTools(this.value,'keyword')" /></div><div class="input-group"><label class="input-label" for="tools-filter">启用状态</label><select class="input" id="tools-filter" onchange="filterTools(this.value,'filter')">${[
      ['all', '全部工具'],
      ['enabled', '已启用'],
      ['disabled', '未启用']
    ]
      .map(
        ([v, t]) =>
          `<option value="${v}" ${tc.filter === v ? 'selected' : ''}>${t}</option>`
      )
      .join(
        ''
      )}</select></div><button class="btn btn-secondary" ${disabled ? 'disabled' : ''} onclick="openToolPackageImport()">上传 / 导入包</button><button class="btn btn-secondary" ${tc.loading || tc.busy ? 'disabled' : ''} onclick="refreshToolsData()">${tc.loading ? '正在读取…' : '刷新'}</button></div>
    <section class="table-container" aria-label="登记工具列表">${
      tools.length
        ? `<div class="table-wrapper"><table class="ops-table tools-table"><thead><tr><th>工具 / 用途</th><th>版本与平台</th><th>文件校验</th><th>平台测试</th><th>启用状态</th><th>操作</th></tr></thead><tbody>${tools
            .map((tool) => {
              const packages = toolPackages(tool);
              return `<tr><td><strong>${escapeHtml(tool.name)}</strong><div class="table-secondary-text">${escapeHtml(tool.id)}</div><p class="tools-purpose">${escapeHtml(tool.purpose)}</p></td><td>${tool.versions.length} 个版本 / ${packages.length} 个包<div class="table-secondary-text">${escapeHtml([...new Set(packages.flatMap((p) => p.tool.platforms.map((platform) => `${platform.os} / ${platform.arch}`)))].join('、'))}</div></td><td>${toolStatusBadge(packages.length > 0 && packages.every((p) => p.file_verified), '校验通过', '待校验')}</td><td>${toolStatusBadge(
                packages.some((p) =>
                  p.tests.some(
                    (t) =>
                      t.status === 'passed' &&
                      (!p.tool.requires_desktop || t.desktop)
                  )
                ),
                '有通过记录',
                packages.some((p) => p.tests.some((t) => t.status === 'failed')) ? '有失败记录' : '尚未验证'
              )}</td><td><label class="tool-permission"><input type="checkbox" aria-label="启用${escapeHtml(tool.name)}" ${tool.enabled ? 'checked' : ''} ${disabled ? 'disabled' : ''} onchange="toolCenterMutation({action:'set_tool_enabled',tool_id:${eventValue(tool.id)},enabled:this.checked},this.checked?'工具已启用':'工具已停用')" />${tool.enabled ? '已启用' : '未启用'}</label></td><td><button class="btn btn-secondary btn-sm" onclick="selectManagedTool(${eventValue(tool.id)})">详情</button> <button class="btn btn-secondary btn-sm" ${disabled ? 'disabled' : ''} onclick="openToolCenterEditor(${eventValue(tool.id)})">编辑</button></td></tr>`;
            })
            .join('')}</tbody></table></div>`
        : renderEmptyState(
            tc.loading
              ? '正在读取工具目录…'
              : snapshot?.tools.length
                ? '没有匹配的工具'
                : '尚未登记工具',
            snapshot?.tools.length
              ? '调整搜索或筛选条件。'
              : '先上传 ZIP 或导入 HTTPS 包，文件校验成功后新增工具资料。'
          )
    }</section>
    ${selected ? renderManagedToolDetails(selected, disabled) : ''}
    ${renderToolSync(snapshot)}
    ${
      snapshot?.audit?.length
        ? `<details class="card tools-audit"><summary>最近工具管理记录</summary><ul>${snapshot.audit
            .slice(0, 20)
            .map(
              (row) =>
                `<li>${escapeHtml(formatApiDate(row.created_at))} · ${escapeHtml(row.actor)} · ${escapeHtml(row.action)} · ${escapeHtml(row.result)}（修订 ${row.revision}）</li>`
            )
            .join('')}</ul></details>`
        : ''
    }
  </div>`;
}

function renderToolSync(snapshot) {
  const nodes = snapshot?.nodes || snapshot?.sync;
  if (!Array.isArray(nodes)) return '';
  return `<section class="card"><h2 class="section-title">节点目录同步</h2>${nodes.length ? `<ul class="tools-sync">${nodes.map((node) => `<li><strong>${escapeHtml(node.hostname || node.agent_instance_id)}</strong> ${node.applied_revision === snapshot.revision ? '已同步' : '未同步'}（已应用 ${node.applied_revision ?? '无'} / 当前 ${snapshot.revision}）${node.supported === false ? '，需要升级 Agent' : ''}</li>`).join('')}</ul>` : '<p class="mcp-note">暂无在线节点。</p>'}</section>`;
}

function browserAdapterFacts(startup) {
  if (startup.interface !== 'stdio_mcp') return { registered: '不适用', effective: startup.interface === 'agent_browser_cli' ? 'agent-browser CLI' : '通用 CLI', source: '独立 CLI', legacy: false };
  const explicit = startup.browser_adapter != null;
  return { registered: explicit ? startup.browser_adapter : '未指定', effective: explicit ? startup.browser_adapter : (startup.browser_executable ? 'playwright' : 'none'), source: explicit ? '显式配置' : '旧默认规则', legacy: !explicit && Boolean(startup.browser_executable) };
}

function renderBrowserAdapter(startup) {
  const facts = browserAdapterFacts(startup);
  return `<dt>登记适配模式</dt><dd>${escapeHtml(facts.registered)}</dd><dt>最终适配模式</dt><dd>${escapeHtml(facts.effective)}（${facts.source}）${facts.legacy ? '<p class="mcp-error">旧登记未指定适配模式，当前按 Playwright 追加 --browser 等参数。agent-browser MCP 请明确设置 browser_adapter 为 agent_browser。</p>' : ''}</dd>`;
}

function browserStartupExample(kind) {
  const startup = { interface: 'stdio_mcp', executable: 'tool.exe', args: [], browser_adapter: 'none', browser_executable: '' };
  if (kind === 'playwright') Object.assign(startup, { executable: 'node/node.exe', args: ['playwright-mcp/cli.js'], browser_adapter: 'playwright', browser_executable: 'browser/chrome-win64/chrome.exe' });
  if (kind === 'agent_browser') Object.assign(startup, { executable: 'bin/agent-browser-win32-x64.exe', args: ['mcp', '--tools', 'core'], browser_adapter: 'agent_browser', browser_executable: 'browser/chrome-win64/chrome.exe' });
  if (kind === 'agent_browser_cli') { Object.assign(startup, { interface: 'agent_browser_cli', executable: 'bin/agent-browser-win32-x64.exe', browser_executable: 'browser/chrome-win64/chrome.exe' }); delete startup.browser_adapter; }
  return { startup, health_check: { method: kind === 'agent_browser_cli' ? 'version' : 'tools/list', timeout_secs: 120 } };
}

function showBrowserStartupExample(kind) {
  const output = document.getElementById('tool-startup-example');
  if (output) output.textContent = JSON.stringify(browserStartupExample(kind), null, 2);
}

function renderManagedToolDetails(tool, disabled) {
  return `<section class="card tools-details"><h2 class="section-title" id="tool-details-title" tabindex="-1">${escapeHtml(tool.name)} · 使用说明</h2><p class="tools-prose">${escapeHtml(tool.instructions)}</p>${tool.examples.length ? `<h3>使用示例</h3><ul>${tool.examples.map((value) => `<li class="tools-prose">${escapeHtml(value)}</li>`).join('')}</ul>` : ''}${tool.dependencies.length ? `<h3>依赖</h3><ul>${tool.dependencies.map((value) => `<li>${escapeHtml(value)}</li>`).join('')}</ul>` : ''}${tool.versions
    .map(
      (version) =>
        `<h3>版本 ${escapeHtml(version.version)}</h3>${version.packages
          .map(
            (pkg) =>
              `<div class="tools-package-detail"><p>${escapeHtml(pkg.tool.description)}</p><dl class="tools-facts"><dt>包来源</dt><dd>${pkg.source.kind === 'hosted' ? '中继托管' : escapeHtml(pkg.source.url)}</dd><dt>大小</dt><dd>${pkg.tool.package.size.toLocaleString()} 字节</dd><dt>SHA-256</dt><dd class="font-mono text-break">${escapeHtml(pkg.tool.package.sha256)}</dd><dt>许可证</dt><dd>${escapeHtml(pkg.tool.license)}</dd><dt>桌面要求</dt><dd>${pkg.tool.requires_desktop ? '需要真实交互桌面' : '无需交互桌面'}</dd><dt>适用平台</dt><dd>${pkg.tool.platforms.map((p) => `${escapeHtml(p.os)} / ${escapeHtml(p.arch)}：${escapeHtml(p.os_versions)}（${escapeHtml(({passed:"有通过记录",pending:"尚未验证",unsupported:"声明不支持"})[p.status] || p.status)}）`).join('<br>')}</dd><dt>已知限制</dt><dd>${pkg.tool.limitations.map(escapeHtml).join('；') || '无登记'}</dd>${renderBrowserAdapter(pkg.tool.startup)}<dt>执行方式</dt><dd>${escapeHtml(pkg.tool.startup.interface)} / ${escapeHtml(pkg.tool.startup.executable)}</dd></dl><p>${toolStatusBadge(pkg.file_verified, '文件校验通过', '文件待校验')} ${toolStatusBadge(
                pkg.tests.some(
                  (t) =>
                    t.status === 'passed' &&
                    (!pkg.tool.requires_desktop || t.desktop)
                ),
                '有平台通过记录',
                '平台尚未验证'
              )}</p>${pkg.tests.length ? `<ul>${pkg.tests.map((t) => `<li>${escapeHtml(t.environment)} / ${t.desktop ? '交互桌面' : '无交互桌面'} / ${escapeHtml(({passed:"通过",pending:"待测",failed:"失败"})[t.status] || t.status)}：${escapeHtml(t.result)}（${escapeHtml(t.tested_at)}）</li>`).join('')}</ul>` : ''}<button class="btn btn-secondary btn-sm" ${disabled ? 'disabled' : ''} onclick="openToolPlatformTest(${eventValue(tool.id)},${eventValue(version.version)},${eventValue(pkg.tool.package.sha256)})">登记实际环境测试</button></div>`
          )
          .join('')}`
    )
    .join('')}</section>`;
}

function packageDefinitionTemplate() {
  const receipt = state.toolCenter.receipt;
  const sha = receipt?.sha256 || '0'.repeat(64);
  return [
    {
      version: '1.0.0',
      packages: [
        {
          tool: {
            id: 'my-tool',
            version: '1.0.0',
            description: '工具的具体用途',
            capabilities: ['utility'],
            documentation_url: 'https://example.com/docs',
            publisher: '工具发布者',
            license: 'MIT',
            limitations: [],
            release_notes: '首次登记',
            permissions: ['execute'],
            requires_desktop: false,
            platforms: [
              {
                os: 'windows',
                arch: 'x86_64',
                os_versions: '实际测试的系统版本',
                status: 'pending',
                min_windows_build: 0,
                max_windows_build: 0
              }
            ],
            package: {
              url:
                receipt?.source.kind === 'external'
                  ? receipt.source.url
                  : `relay://packages/${sha}`,
              size: receipt?.size || 1,
              sha256: sha,
              format: 'zip'
            },
            startup: {
              interface: 'cli',
              executable: 'tool.exe',
              args: [],
              browser_executable: '',
              operations: [
                {
                  name: 'version',
                  description: '读取版本',
                  input_schema: {
                    type: 'object',
                    properties: {},
                    additionalProperties: false
                  },
                  args: [{ kind: 'literal', value: '--version' }],
                  result_format: 'text'
                }
              ]
            },
            health_check: { method: 'schema', timeout_secs: 30 },
            timeout_secs: 60
          },
          source: receipt?.source || { kind: 'hosted' },
          file_verified: false,
          tests: []
        }
      ]
    }
  ];
}

function openToolCenterEditor(id) {
  const tc = state.toolCenter;
  if (tc.busy || tc.loading || !tc.snapshot || state.demoMode) return;
  const existing = tc.snapshot.tools.find((tool) => tool.id === id);
  tc.draft = existing
    ? structuredClone(existing)
    : {
        id: '',
        name: '',
        purpose: '',
        instructions: '',
        examples: [],
        dependencies: [],
        enabled: false,
        versions: packageDefinitionTemplate()
      };
  tc.draft.versionsText = JSON.stringify(tc.draft.versions, null, 2);
  tc.draft.examplesText = tc.draft.examples.join('\n');
  tc.draft.dependenciesText = tc.draft.dependencies.join('\n');
  tc.error = '';
  openModal('toolCenterEdit', { existing: Boolean(existing) });
}
function openToolPackageImport() {
  if (state.toolCenter.busy || state.demoMode) return;
  state.toolCenter.draft = { source: 'hosted', url: '', expected_sha256: '' };
  state.toolCenter.receipt = null;
  state.toolCenter.error = '';
  state.toolCenter.progress = '';
  openModal('toolCenterPackage');
}
function openToolPlatformTest(id, version, sha256) {
  state.toolCenter.draft = {
    os: 'windows',
    arch: 'x86_64',
    environment: '',
    desktop: false,
    status: 'pending',
    result: '',
    tested_at: new Date().toISOString()
  };
  state.toolCenter.error = '';
  openModal('toolCenterTest', { id, version, sha256 });
}
function updateToolDraft(key, value) {
  state.toolCenter.draft[key] = value;
}
function toolField(key, label, options = {}) {
  const value = state.toolCenter.draft?.[key] ?? '';
  return `<div class="input-group ${options.wide ? 'tools-wide' : ''}"><label class="input-label" for="tool-${key}">${label}</label>${options.multiline ? `<textarea id="tool-${key}" class="input ${options.code ? 'font-mono tools-code' : ''}" rows="${options.rows || 3}" ${options.required ? 'required' : ''} ${options.disabled ? 'disabled' : ''} spellcheck="false" oninput="updateToolDraft('${key}',this.value)">${escapeHtml(value)}</textarea>` : `<input id="tool-${key}" class="input" value="${escapeHtml(value)}" ${options.required ? 'required' : ''} ${options.disabled ? 'disabled' : ''} ${options.type ? `type="${options.type}"` : ''} oninput="updateToolDraft('${key}',this.value)" />`}</div>`;
}

function renderToolCenterModal() {
  const tc = state.toolCenter,
    d = tc.draft || {},
    busy = tc.busy;
  const head = (title) =>
    `<div class="modal-header"><h2 class="modal-title" id="tool-modal-title">${title}</h2><button class="btn btn-ghost" aria-label="关闭弹窗" ${busy ? 'disabled' : ''} onclick="closeModal()">✕</button></div>`;
  const error = tc.error
    ? `<p class="mcp-error" role="alert" tabindex="-1" id="tool-form-error">${escapeHtml(tc.error)}</p>`
    : '';
  const footer = (action, label) =>
    `<div class="modal-footer"><button type="button" class="btn btn-secondary" ${busy ? 'disabled' : ''} onclick="closeModal()">取消</button><button type="submit" class="btn btn-primary" ${busy || state.demoMode ? 'disabled' : ''}>${busy ? '正在处理…' : label}</button></div>`;
  if (state.activeModal === 'toolCenterEdit')
    return `<div class="modal tools-editor" role="dialog" aria-modal="true" aria-labelledby="tool-modal-title">${head(state.modalTargetData.existing ? '编辑工具资料' : '新增工具')}<form onsubmit="saveManagedTool(event)"><div class="modal-body"><details class="card"><summary>浏览器启动配置示例</summary><p class="mcp-note">以下仅为启动字段示例，请按实际包路径更新对应平台包。修改启动配置会使旧测试记录失效。</p><label class="input-label" for="tool-startup-kind">接口与适配模式</label><select class="input" id="tool-startup-kind" onchange="showBrowserStartupExample(this.value)"><option value="none">通用 MCP</option><option value="playwright">Playwright MCP</option><option value="agent_browser">agent-browser MCP</option><option value="agent_browser_cli">agent-browser CLI</option></select><pre class="tools-prose font-mono" id="tool-startup-example">${escapeHtml(JSON.stringify(browserStartupExample('none'), null, 2))}</pre></details><fieldset class="tools-fields" ${busy ? 'disabled' : ''}><div class="tools-form-grid">${toolField('id', '工具标识', { required: true, disabled: state.modalTargetData.existing })}${toolField('name', '工具名称', { required: true })}${toolField('purpose', '用途', { required: true, wide: true, multiline: true })}${toolField('instructions', '使用说明', { required: true, wide: true, multiline: true })}${toolField('examplesText', '使用示例（每行一项）', { multiline: true })}${toolField('dependenciesText', '依赖（每行一项）', { multiline: true })}</div><details class="tools-definition" open><summary>版本、平台包与执行定义</summary><p class="mcp-note">每个版本可以包含多个平台包；填写包回执中的大小和摘要。相同版本的已有包不可替换，新增内容请使用新版本。平台范围只登记实际验证的系统；上传成功不会自动变为测试通过。</p>${toolField('versionsText', '版本与平台包定义（JSON）', { required: true, multiline: true, code: true, rows: 16 })}<p class="mcp-note">CLI 操作需登记参数 Schema、参数数组及文本 / JSON 结果格式；通用 STDIO MCP 使用 initialize、tools/list、tools/call。启动参数按数组传递。</p></details></fieldset>${error}</div>${footer('', '保存工具资料')}</form></div>`;
  if (state.activeModal === 'toolCenterPackage')
    return `<div class="modal tools-editor" role="dialog" aria-modal="true" aria-labelledby="tool-modal-title">${head('上传或导入工具包')}<form onsubmit="importToolPackage(event)"><div class="modal-body"><fieldset class="tools-fields" ${busy ? 'disabled' : ''}><div class="input-group"><label class="input-label" for="tool-package-source">包来源</label><select id="tool-package-source" class="input" onchange="updateToolDraft('source',this.value);renderModal()"><option value="hosted" ${d.source === 'hosted' ? 'selected' : ''}>上传 ZIP 到中继</option><option value="external" ${d.source === 'external' ? 'selected' : ''}>外部 HTTPS 链接</option></select></div>${d.source === 'hosted' ? '<div class="input-group"><label class="input-label" for="tool-zip">ZIP 文件（最大 1 GiB）</label><input class="input" id="tool-zip" type="file" accept=".zip,application/zip" required /></div>' : `${toolField('url', 'HTTPS ZIP 地址（支持签名查询参数）', { required: true, type: 'url' })}${toolField('expected_sha256', '预期 SHA-256（可选）')}`}</fieldset><p class="mcp-note">中继只校验文件，不执行包内程序。外链包仅临时读取；运行时由 Agent 直接下载。签名参数不在普通列表展示。</p>${tc.progress ? `<p role="status">${escapeHtml(tc.progress)}</p>` : ''}${tc.receipt ? `<div class="tools-receipt" role="status"><strong>文件校验通过，平台尚未测试</strong><p>${tc.receipt.size.toLocaleString()} 字节</p><p class="font-mono text-break">${escapeHtml(tc.receipt.sha256)}</p><button class="btn btn-secondary" type="button" onclick="closeModal();openToolCenterEditor()">使用此包新增工具</button></div>` : ''}${error}</div>${footer('', '校验并保存包')}</form></div>`;
  if (state.activeModal === 'toolCenterTest')
    return `<div class="modal tools-editor" role="dialog" aria-modal="true" aria-labelledby="tool-modal-title">${head('登记实际环境测试')}<form onsubmit="saveToolPlatformTest(event)"><div class="modal-body"><fieldset class="tools-fields" ${busy ? 'disabled' : ''}><div class="tools-form-grid">${toolField('os', '操作系统', { required: true })}${toolField('arch', '架构', { required: true })}${toolField('environment', '实际系统版本与测试环境', { required: true, wide: true, multiline: true })}<div class="input-group"><label class="tool-permission"><input type="checkbox" ${d.desktop ? 'checked' : ''} onchange="updateToolDraft('desktop',this.checked)" /> 使用真实交互桌面</label></div><div class="input-group"><label class="input-label" for="tool-test-status">测试结果</label><select id="tool-test-status" class="input" onchange="updateToolDraft('status',this.value)">${[
      ['pending', '待测试'],
      ['passed', '通过'],
      ['failed', '失败']
    ]
      .map(
        ([v, t]) =>
          `<option value="${v}" ${d.status === v ? 'selected' : ''}>${t}</option>`
      )
      .join(
        ''
      )}</select></div>${toolField('result', '验证步骤与实际结果', { required: true, wide: true, multiline: true })}${toolField('tested_at', '测试时间（RFC 3339）', { required: true, wide: true })}</div></fieldset><p class="mcp-note">headless 结果不能作为交互桌面通过；各 Windows build 独立记录，登记测试不会启用工具。</p>${error}</div>${footer('', '保存测试记录')}</form></div>`;
  return '';
}

async function toolFormRun(work) {
  const tc = state.toolCenter;
  if (tc.busy || state.demoMode) return;
  const epoch = tc.epoch;
  tc.busy = true;
  tc.error = '';
  renderModal();
  try {
    await work();
  } catch (error) {
    if (toolsContext(epoch)) tc.error = error.message;
  } finally {
    tc.busy = false;
    if (toolsContext(epoch)) {
      renderModal();
      if (tc.error) document.getElementById('tool-form-error')?.focus();
      else {
        await refreshToolsData();
      }
    } else if (state.isLoggedIn && state.currentTab === 'tools') {
      await refreshToolsData();
    }
  }
}
function sameStartup(left, right) {
  const normalized = value => ({ interface: value.interface, executable: value.executable, args: value.args, browser_executable: value.browser_executable || '', browser_adapter: value.browser_adapter ?? null, operations: value.operations || [] });
  return JSON.stringify(normalized(left)) === JSON.stringify(normalized(right));
}

async function saveManagedTool(event) {
  event.preventDefault();
  const tc = state.toolCenter;
  let tool;
  try {
    const d = tc.draft;
    const versions = JSON.parse(d.versionsText);
    if (!Array.isArray(versions) || !versions.length) throw new Error();
    versions.forEach((version) =>
      version.packages.forEach((pkg) => {
        pkg.tool.id = d.id.trim();
        pkg.tool.version = version.version;
      })
    );
    const existing = tc.snapshot.tools.find(value => value.id === d.id.trim());
    for (const version of versions) for (const pkg of version.packages) {
      const startup = pkg.tool.startup;
      const previous = existing?.versions.find(value => value.version === version.version)?.packages.find(value => value.tool.package.sha256 === pkg.tool.package.sha256 && JSON.stringify(value.tool.platforms.map(p => [p.os, p.arch])) === JSON.stringify(pkg.tool.platforms.map(p => [p.os, p.arch])));
      if (startup.interface === 'stdio_mcp' && startup.browser_executable && startup.browser_adapter == null && (!previous || !sameStartup(previous.tool.startup, startup))) {
        tc.error = '浏览器 MCP 新增或启动配置变更必须显式设置 browser_adapter 为 playwright 或 agent_browser。';
        renderModal();
        return;
      }
    }
    tool = {
      id: d.id.trim(),
      name: d.name.trim(),
      purpose: d.purpose.trim(),
      instructions: d.instructions.trim(),
      examples: d.examplesText
        .split('\n')
        .map((x) => x.trim())
        .filter(Boolean),
      dependencies: d.dependenciesText
        .split('\n')
        .map((x) => x.trim())
        .filter(Boolean),
      enabled: d.enabled,
      versions
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
      '请填写完整资料，并检查版本与平台包 JSON；工具标识仅允许字母、数字、点、下划线、加减号。';
    renderModal();
    return;
  }
  await toolFormRun(async () => {
    await toolCenterCommand({ action: 'put_tool', tool });
    tc.busy = false;
    closeModal();
    showToast('工具资料已保存；启用状态请单独设置', 'success');
  });
}
async function saveToolPlatformTest(event) {
  event.preventDefault();
  const tc = state.toolCenter,
    target = state.modalTargetData;
  const record = structuredClone(tc.draft);
  if (
    !record.environment.trim() ||
    !record.result.trim() ||
    Number.isNaN(Date.parse(record.tested_at))
  ) {
    tc.error = '请填写实际环境、验证结果与有效时间。';
    renderModal();
    return;
  }
  await toolFormRun(async () => {
    await toolCenterCommand({
      action: 'record_test',
      tool_id: target.id,
      version: target.version,
      sha256: target.sha256,
      record
    });
    tc.busy = false;
    closeModal();
    showToast('测试记录已保存', 'success');
  });
}

async function importToolPackage(event) {
  event.preventDefault();
  const tc = state.toolCenter,
    d = structuredClone(tc.draft);
  const file = document.getElementById('tool-zip')?.files[0];
  if (
    d.source === 'hosted' &&
    (!file || file.size < 1 || file.size > 1024 * 1024 * 1024)
  ) {
    tc.error = '请选择 1 字节至 1 GiB 的 ZIP 文件。';
    renderModal();
    return;
  }
  if (d.source === 'external') {
    try {
      const u = new URL(d.url);
      if (u.protocol !== 'https:' || u.username || u.password || u.hash)
        throw new Error();
      if (d.expected_sha256 && !/^[0-9a-f]{64}$/.test(d.expected_sha256))
        throw new Error();
    } catch (_) {
      tc.error =
        '请填写不含用户信息与片段的 HTTPS 地址，以及有效的小写 SHA-256。';
      renderModal();
      return;
    }
  }
  const epoch = tc.epoch;
  await toolFormRun(async () => {
    if (d.source === 'external') {
      tc.progress = '正在临时读取并校验 HTTPS 包…';
      renderModal();
      tc.receipt = await toolCenterCommand({
        action: 'import_external',
        url: d.url,
        expected_sha256: d.expected_sha256 || null
      });
    } else {
      tc.progress = '正在计算文件 SHA-256…';
      renderModal();
      const hash = new ToolPackageSha256();
      for (let offset = 0; offset < file.size; offset += 256 * 1024) {
        if (!toolsContext(epoch)) throw new Error('管理会话已变化');
        hash.update(
          new Uint8Array(
            await file.slice(offset, offset + 256 * 1024).arrayBuffer()
          )
        );
        await new Promise((resolve) => setTimeout(resolve, 0));
      }
      const sha256 = hash.digest();
      const upload = await toolCenterCommand({
        action: 'begin_upload',
        size: file.size,
        sha256
      });
      let offset = 0;
      try {
        while (offset < file.size) {
          if (!toolsContext(epoch)) throw new Error('管理会话已变化');
          const bytes = new Uint8Array(
            await file.slice(offset, offset + 256 * 1024).arrayBuffer()
          );
          let binary = '';
          for (let i = 0; i < bytes.length; i += 8192)
            binary += String.fromCharCode(...bytes.subarray(i, i + 8192));
          const acknowledgement = await toolCenterCommand({
            action: 'upload_chunk',
            upload_id: upload.upload_id,
            offset,
            data_base64: btoa(binary)
          });
          offset += bytes.length;
          if (acknowledgement.offset !== offset)
            throw new Error('上传偏移未确认，已停止传输');
          tc.progress = `正在上传 ${Math.floor((offset / file.size) * 100)}%（每块独立确认）`;
          renderModal();
        }
        tc.progress = '正在校验 ZIP 与完整摘要…';
        renderModal();
        tc.receipt = await toolCenterCommand({
          action: 'finish_upload',
          upload_id: upload.upload_id
        });
      } catch (error) {
        if (toolsContext(epoch)) {
          try {
            await toolCenterCommand({
              action: 'cancel_upload',
              upload_id: upload.upload_id
            });
          } catch (_) {}
        }
        throw error;
      }
    }
    tc.progress = '文件校验完成；请登记资料与实际测试后再启用。';
  });
}

// 有界增量摘要，避免为最大 1 GiB 文件创建同等大小的浏览器内存副本。
class ToolPackageSha256 {
  constructor() {
    this.h = new Uint32Array([
      0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c,
      0x1f83d9ab, 0x5be0cd19
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
      Math.floor(bits / 0x100000000)
    );
    new DataView(this.buffer.buffer).setUint32(60, bits >>> 0);
    this.block();
    return Array.from(this.h, (v) => v.toString(16).padStart(8, '0')).join('');
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
  0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2
]);

document.addEventListener('keydown', (event) => {
  if (event.key !== 'Tab' || !state.activeModal?.startsWith('toolCenter'))
    return;
  const dialog = document.querySelector('#modal-backdrop .modal');
  const fields = Array.from(
    dialog?.querySelectorAll(
      'button:not(:disabled),input:not(:disabled),select:not(:disabled),textarea:not(:disabled),summary,[tabindex="0"]'
    ) || []
  ).filter((el) => el.getClientRects().length);
  if (!fields.length) return;
  const first = fields[0],
    last = fields[fields.length - 1];
  if (event.shiftKey && document.activeElement === first) {
    event.preventDefault();
    last.focus();
  } else if (!event.shiftKey && document.activeElement === last) {
    event.preventDefault();
    first.focus();
  }
});
