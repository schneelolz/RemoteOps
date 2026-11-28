<script setup lang="ts">
import { computed, nextTick } from "vue";
import * as c from "../services/operations";
import ModalShell from "./ModalShell.vue";
const tc = c.state.toolCenter;
const tool = computed(() =>
  tc.snapshot?.tools.find((item) => item.id === tc.selected),
);
const version = computed(
  () =>
    tool.value?.versions.find((item) => item.version === tc.detailVersion) ||
    tool.value?.versions[0],
);
const tabs = [
  ["guide", "使用指南"],
  ["packages", "版本与平台包"],
  ["tests", "测试记录"],
];
const statusLabels: Record<string, string> = {
  passed: "通过",
  failed: "失败",
  pending: "待测试",
  unsupported: "明确不支持",
};
async function tabKey(event: KeyboardEvent, index: number) {
  const next =
    event.key === "ArrowRight"
      ? (index + 1) % tabs.length
      : event.key === "ArrowLeft"
        ? (index + tabs.length - 1) % tabs.length
        : event.key === "Home"
          ? 0
          : event.key === "End"
            ? tabs.length - 1
            : null;
  if (next === null) return;
  event.preventDefault();
  tc.detailTab = tabs[next][0];
  await nextTick();
  document.getElementById("tool-detail-tab-" + tc.detailTab)?.focus();
}
</script>
<template>
  <ModalShell :title="tool?.name || '工具详情'" wide @close="c.closeModal()">
    <template v-if="tool">
      <div class="tool-details-subtitle">
        <span class="font-mono">{{ tool.id }}</span
        ><span
          class="badge"
          :class="tool.enabled ? 'badge-online' : 'badge-info'"
          >{{ tool.enabled ? "已启用" : "未启用" }}</span
        >
      </div>
      <div class="tools-editor-tabs" role="tablist" aria-label="工具详情分组">
        <button
          v-for="(tab, index) in tabs"
          :id="'tool-detail-tab-' + tab[0]"
          :key="tab[0]"
          type="button"
          role="tab"
          :aria-selected="tc.detailTab === tab[0]"
          aria-controls="tool-detail-panel"
          :tabindex="tc.detailTab === tab[0] ? 0 : -1"
          @click="tc.detailTab = tab[0]"
          @keydown="tabKey($event, index)"
        >
          {{ tab[1] }}
        </button>
      </div>
      <div v-if="tc.detailTab !== 'guide'" class="tool-details-version">
        <label for="tool-detail-version" class="input-label">工具版本</label
        ><select
          id="tool-detail-version"
          v-model="tc.detailVersion"
          class="input"
        >
          <option
            v-for="item in tool.versions"
            :key="item.version"
            :value="item.version"
          >
            {{ item.version }}
          </option>
        </select>
      </div>
      <div
        id="tool-detail-panel"
        class="modal-body tool-details-body"
        role="tabpanel"
        :aria-labelledby="'tool-detail-tab-' + tc.detailTab"
      >
        <template v-if="tc.detailTab === 'guide'"
          ><section>
            <h3>用途</h3>
            <p class="tools-prose">{{ tool.purpose }}</p>
          </section>
          <section>
            <h3>使用说明</h3>
            <p class="tools-prose">{{ tool.instructions }}</p>
          </section>
          <section>
            <h3>使用示例</h3>
            <ul v-if="tool.examples.length">
              <li v-for="(example, index) in tool.examples" :key="index">
                <code>{{ example }}</code>
              </li>
            </ul>
            <p v-else class="mcp-note">尚未填写使用示例。</p>
          </section>
          <section>
            <h3>依赖</h3>
            <ul v-if="tool.dependencies.length">
              <li v-for="(dependency, index) in tool.dependencies" :key="index">
                {{ dependency }}
              </li>
            </ul>
            <p v-else class="mcp-note">未登记额外依赖。</p>
          </section></template
        >
        <template v-else-if="version"
          ><section
            v-for="(pkg, index) in version.packages"
            :key="index"
            class="tool-details-package"
          >
            <h3>平台包 {{ index + 1 }}</h3>
            <template v-if="tc.detailTab === 'packages'"
              ><dl class="tools-facts">
                <dt>平台范围</dt>
                <dd>
                  <div v-for="(platform, i) in pkg.tool.platforms" :key="i">
                    {{ platform.os }} / {{ platform.arch }}
                    {{ platform.os_versions || "" }}
                  </div>
                </dd>
                <dt>文件摘要</dt>
                <dd class="font-mono">{{ pkg.tool.package.sha256 }}</dd>
                <dt>文件大小</dt>
                <dd>{{ pkg.tool.package.size.toLocaleString() }} 字节</dd>
                <dt>文件校验</dt>
                <dd>
                  <span
                    class="badge"
                    :class="pkg.file_verified ? 'badge-online' : 'badge-info'"
                    >{{ pkg.file_verified ? "校验通过" : "待校验" }}</span
                  >
                </dd>
                <dt>执行接口</dt>
                <dd>{{ pkg.tool.startup.interface }}</dd>
                <dt>适配模式</dt>
                <dd>
                  {{ c.browserAdapterFacts(pkg.tool.startup).effective }}（{{
                    c.browserAdapterFacts(pkg.tool.startup).source
                  }}）
                </dd>
              </dl>
              <p
                v-if="c.browserAdapterFacts(pkg.tool.startup).legacy"
                class="mcp-error"
              >
                旧登记未指定适配模式，当前按 Playwright 追加 --browser
                等参数；agent-browser MCP 请显式设置 browser_adapter。
              </p></template
            ><template v-else
              ><p class="mcp-note">
                {{
                  pkg.tool.platforms
                    .map((platform) => platform.os + " / " + platform.arch)
                    .join("、")
                }}；测试仅覆盖记录中的实际环境及接口。
              </p>
              <article
                v-for="(test, i) in pkg.tests"
                :key="i"
                class="tool-test-record"
              >
                <div class="action-row">
                  <strong>{{ test.os }} / {{ test.arch }}</strong
                  ><span
                    class="badge"
                    :class="
                      test.status === 'passed'
                        ? 'badge-online'
                        : test.status === 'failed'
                          ? 'badge-danger'
                          : 'badge-info'
                    "
                    >{{ statusLabels[test.status] || test.status }}</span
                  ><span>{{ test.desktop ? "交互桌面" : "无头" }}</span>
                </div>
                <p class="tools-prose">{{ test.environment }}</p>
                <p class="tools-prose">{{ test.result }}</p>
                <time class="mcp-note">{{
                  c.formatApiDate(test.tested_at)
                }}</time>
              </article>
              <p v-if="!pkg.tests.length" class="mcp-note">
                尚未验证。文件校验通过不代表当前平台可正常运行。
              </p>
              <button
                class="btn btn-secondary"
                :disabled="tc.busy || tc.loading"
                @click="
                  c.openToolPlatformTest(
                    tool.id,
                    version.version,
                    pkg.tool.package.sha256,
                  )
                "
              >
                登记实际环境测试
              </button></template
            >
          </section>
          <p v-if="!version.packages.length" class="mcp-note">
            该版本尚未登记平台包。
          </p></template
        >
        <p v-else class="mcp-note">尚未登记工具版本。</p>
      </div>
      <footer class="modal-footer">
        <button type="button" class="btn btn-secondary" @click="c.closeModal()">
          关闭</button
        ><button
          type="button"
          class="btn btn-primary"
          :disabled="tc.busy || tc.loading"
          @click="c.openToolCenterEditor(tool.id)"
        >
          编辑工具资料
        </button>
      </footer>
    </template>
    <div v-else class="modal-body"><p>工具已不存在，请关闭后刷新列表。</p></div>
  </ModalShell>
</template>
