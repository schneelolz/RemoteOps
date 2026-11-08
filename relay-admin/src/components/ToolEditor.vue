<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import * as c from "../services/operations";
import type { ToolVersion } from "../models";
import ModalShell from "./ModalShell.vue";
const tc = c.state.toolCenter,
  kind = computed(() => c.state.activeModal),
  d = computed(() => tc.draft);
const tabs = [
  ["basic", "基本资料"],
  ["packages", "版本与平台包"],
  ["advanced", "高级定义"],
];
const example = ref("none");
const exampleText = computed(() =>
  JSON.stringify(c.browserStartupExample(example.value), null, 2),
);
const versions = computed(() => {
  try {
    const v = JSON.parse(d.value?.versionsText || "[]");
    return Array.isArray(v) ? (v as ToolVersion[]) : null;
  } catch {
    return null;
  }
});
const title = computed(() =>
  kind.value === "toolCenterEdit"
    ? c.state.modalTargetData?.existing
      ? "编辑工具资料"
      : "新增工具"
    : kind.value === "toolCenterPackage"
      ? "上传或导入工具包"
      : "登记实际环境测试",
);
const fields = [
  ["id", "工具标识"],
  ["name", "工具名称"],
  ["purpose", "用途"],
  ["instructions", "使用说明"],
  ["examplesText", "使用示例（每行一项）"],
  ["dependenciesText", "依赖（每行一项）"],
] as const;
async function tabKey(event: KeyboardEvent, index: number) {
  const n =
    event.key === "ArrowRight"
      ? (index + 1) % 3
      : event.key === "ArrowLeft"
        ? (index + 2) % 3
        : event.key === "Home"
          ? 0
          : event.key === "End"
            ? 2
            : null;
  if (n !== null) {
    event.preventDefault();
    tc.editorTab = tabs[n][0];
    await nextTick();
    document.getElementById("tool-editor-tab-" + tabs[n][0])?.focus();
  }
}
watch(
  () => tc.error,
  async (error) => {
    if (error) {
      await nextTick();
      document.getElementById("tool-form-error")?.focus();
    }
  },
);
</script>
<template>
  <ModalShell :title="title" wide :busy="tc.busy" @close="c.closeModal()"
    ><form
      v-if="kind === 'toolCenterEdit' && d"
      novalidate
      @submit="c.saveManagedTool"
    >
      <div class="tools-editor-tabs" role="tablist" aria-label="工具资料分组">
        <button
          v-for="(tab, i) in tabs"
          :id="'tool-editor-tab-' + tab[0]"
          :key="tab[0]"
          role="tab"
          type="button"
          :aria-selected="tc.editorTab === tab[0]"
          aria-controls="tool-editor-panel"
          :tabindex="tc.editorTab === tab[0] ? 0 : -1"
          :disabled="tc.busy"
          @click="tc.editorTab = tab[0]"
          @keydown="tabKey($event, i)"
        >
          {{ tab[1] }}
        </button>
      </div>
      <div
        id="tool-editor-panel"
        class="modal-body"
        role="tabpanel"
        :aria-labelledby="'tool-editor-tab-' + tc.editorTab"
      >
        <fieldset class="tools-fields" :disabled="tc.busy">
          <div v-if="tc.editorTab === 'basic'" class="tools-form-grid">
            <div
              v-for="(field, i) in fields"
              :key="field[0]"
              class="input-group"
              :class="{ 'tools-wide': i === 2 || i === 3 }"
            >
              <label class="input-label" :for="'tool-' + field[0]">{{
                field[1]
              }}</label
              ><input
                v-if="i < 2"
                :id="'tool-' + field[0]"
                v-model="d[field[0]]"
                class="input"
                :disabled="i === 0 && c.state.modalTargetData?.existing"
                required
              /><textarea
                v-else
                :id="'tool-' + field[0]"
                v-model="d[field[0]]"
                class="input"
                :rows="i === 2 ? 3 : 5"
                :required="i < 4"
                spellcheck="false"
              />
            </div>
          </div>
          <template v-else-if="tc.editorTab === 'packages'"
            ><p class="mcp-note">
              相同版本已有包不可替换；新增内容请使用新版本。上传及文件校验不会自动登记平台测试通过。
            </p>
            <p v-if="!versions" class="mcp-error">
              版本定义不是有效 JSON，请在高级定义中修正。
            </p>
            <section
              v-for="v in versions"
              :key="v.version"
              class="tools-package-detail"
            >
              <h3>版本 {{ v.version }}</h3>
              <dl
                v-for="p in v.packages"
                :key="p.tool?.package?.sha256"
                class="tools-facts"
              >
                <dt>平台</dt>
                <dd>
                  {{
                    (p.tool?.platforms || [])
                      .map((x) => x.os + " / " + x.arch)
                      .join("、")
                  }}
                </dd>
                <dt>执行接口</dt>
                <dd>{{ p.tool?.startup?.interface || "未填写" }}</dd>
                <dt>文件校验</dt>
                <dd>{{ p.file_verified ? "校验通过" : "待校验" }}</dd>
                <dt>SHA-256</dt>
                <dd class="font-mono">
                  {{ p.tool?.package?.sha256 || "未填写" }}
                </dd>
                <dt>测试记录</dt>
                <dd>{{ p.tests?.length || 0 }} 条，具体覆盖范围见工具详情</dd>
              </dl>
            </section>
            <button
              type="button"
              class="btn btn-secondary"
              @click="tc.editorTab = 'advanced'"
            >
              编辑版本与执行定义
            </button></template
          ><template v-else
            ><p class="mcp-note">
              维护版本、平台包与执行定义。平台范围只登记实际验证的系统，修改启动配置会使旧测试记录失效。
            </p>
            <label for="tool-versionsText" class="input-label"
              >版本与平台包定义（JSON）</label
            ><textarea
              id="tool-versionsText"
              v-model="d.versionsText"
              class="input font-mono tools-code"
              rows="18"
              spellcheck="false"
            />
            <details class="tools-definition">
              <summary>浏览器启动配置示例</summary>
              <label class="input-label" for="tool-startup-kind"
                >接口与适配模式</label
              ><select id="tool-startup-kind" v-model="example" class="input">
                <option value="none">通用 MCP</option>
                <option value="playwright">Playwright MCP</option>
                <option value="agent_browser">agent-browser MCP</option>
                <option value="agent_browser_cli">agent-browser CLI</option>
              </select>
              <pre class="tools-prose font-mono">{{ exampleText }}</pre>
            </details></template
          >
        </fieldset>
        <p
          v-if="tc.error"
          id="tool-form-error"
          class="mcp-error"
          role="alert"
          tabindex="-1"
        >
          {{ tc.error }}
        </p>
      </div>
      <footer class="modal-footer">
        <span class="editor-save-note">一次保存全部页签的修改</span
        ><button
          type="button"
          class="btn btn-secondary"
          :disabled="tc.busy"
          @click="c.closeModal()"
        >
          取消</button
        ><button type="submit" class="btn btn-primary" :disabled="tc.busy">
          {{ tc.busy ? "正在保存…" : "保存工具资料" }}
        </button>
      </footer>
    </form>
    <form
      v-else-if="kind === 'toolCenterPackage' && d"
      @submit="c.importToolPackage"
    >
      <div class="modal-body">
        <fieldset class="tools-fields" :disabled="tc.busy">
          <label for="tool-package-source" class="input-label">包来源</label
          ><select id="tool-package-source" v-model="d.source" class="input">
            <option value="hosted">上传 ZIP 到中继</option>
            <option value="external">外部 HTTPS 链接</option>
          </select>
          <div v-if="d.source === 'hosted'" class="input-group">
            <label for="tool-zip" class="input-label"
              >ZIP 文件（最大 1 GiB）</label
            ><input
              id="tool-zip"
              type="file"
              class="input"
              accept=".zip,application/zip"
              required
            />
          </div>
          <template v-else
            ><label for="tool-url" class="input-label"
              >HTTPS ZIP 地址（支持签名参数）</label
            ><input
              id="tool-url"
              v-model="d.url"
              type="url"
              class="input"
              required /><label for="tool-expected_sha256" class="input-label"
              >预期 SHA-256（可选）</label
            ><input
              id="tool-expected_sha256"
              v-model="d.expected_sha256"
              class="input font-mono"
          /></template>
        </fieldset>
        <p class="mcp-note">
          中继只校验文件，不执行包内程序。外链运行时由 Agent 直接下载。
        </p>
        <p v-if="tc.progress" role="status">{{ tc.progress }}</p>
        <div v-if="tc.receipt" class="tools-receipt">
          <strong>文件校验通过，平台尚未测试</strong>
          <p>{{ tc.receipt.size.toLocaleString() }} 字节</p>
          <p class="font-mono text-break">{{ tc.receipt.sha256 }}</p>
          <button
            type="button"
            class="btn btn-secondary"
            @click="
              c.closeModal();
              c.openToolCenterEditor();
            "
          >
            使用此包新增工具
          </button>
        </div>
        <p
          v-if="tc.error"
          id="tool-form-error"
          class="mcp-error"
          role="alert"
          tabindex="-1"
        >
          {{ tc.error }}
        </p>
      </div>
      <footer class="modal-footer">
        <button
          type="button"
          class="btn btn-secondary"
          :disabled="tc.busy"
          @click="c.closeModal()"
        >
          取消</button
        ><button class="btn btn-primary" :disabled="tc.busy">
          {{ tc.busy ? "正在校验…" : "校验并保存包" }}
        </button>
      </footer>
    </form>
    <form v-else-if="d" @submit="c.saveToolPlatformTest">
      <div class="modal-body">
        <fieldset class="tools-fields" :disabled="tc.busy">
          <div class="tools-form-grid">
            <div
              v-for="field in [
                ['os', '操作系统'],
                ['arch', '架构'],
                ['environment', '实际系统版本与测试环境'],
                ['result', '验证步骤与实际结果'],
                ['tested_at', '测试时间（RFC 3339）'],
              ] as const"
              :key="field[0]"
              class="input-group"
              :class="{ 'tools-wide': !['os', 'arch'].includes(field[0]) }"
            >
              <label class="input-label" :for="'tool-test-' + field[0]">{{
                field[1]
              }}</label
              ><textarea
                v-if="['environment', 'result'].includes(field[0])"
                :id="'tool-test-' + field[0]"
                v-model="d[field[0]]"
                class="input"
                rows="4"
                required
              /><input
                v-else
                :id="'tool-test-' + field[0]"
                v-model="d[field[0]]"
                class="input"
                required
              />
            </div>
          </div>
          <label class="tool-permission"
            ><input
              v-model="d.desktop"
              type="checkbox"
            />使用真实交互桌面</label
          ><label for="tool-test-status" class="input-label">测试结果</label
          ><select id="tool-test-status" v-model="d.status" class="input">
            <option value="pending">待测试</option>
            <option value="passed">通过</option>
            <option value="failed">失败</option>
          </select>
        </fieldset>
        <p class="mcp-note">
          无头结果不能代表交互桌面通过；各 Windows build
          独立记录，登记测试不会启用工具。
        </p>
        <p
          v-if="tc.error"
          id="tool-form-error"
          class="mcp-error"
          role="alert"
          tabindex="-1"
        >
          {{ tc.error }}
        </p>
      </div>
      <footer class="modal-footer">
        <button
          type="button"
          class="btn btn-secondary"
          :disabled="tc.busy"
          @click="c.closeModal()"
        >
          取消</button
        ><button class="btn btn-primary" :disabled="tc.busy">
          {{ tc.busy ? "正在保存…" : "保存测试记录" }}
        </button>
      </footer>
    </form></ModalShell
  >
</template>
