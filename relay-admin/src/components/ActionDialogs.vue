<script setup lang="ts">
import { computed, ref, watch } from "vue";
import * as c from "../services/operations";
import ModalShell from "./ModalShell.vue";
import ToolEditor from "./ToolEditor.vue";
import ToolDetails from "./ToolDetails.vue";
const kind = computed(() => c.state.activeModal),
  data = computed(() => c.state.modalTargetData),
  stop = ref("");
watch(kind, () => {
  stop.value = "";
});
const title = computed(
  () =>
    ({
      mcpCreate: "新建一次性设置",
      mcpFresh: "一次性设置已生成",
      mcpRename: "修改客户端名称",
      mcpRevoke: "撤销客户端接入",
      mcpLegacyDisable: "关闭旧 Token 客户端接入？",
      terminateSession: "确认关闭会话",
      emergencyStop: "确认紧急停止",
      shutdownAgent: "关闭 Agent",
      shutdownAgents: "关闭 Agent",
      purgeClosedSessions: "清除已关闭会话记录",
    })[kind.value as string] || "确认操作",
);
const renameBusy = computed(
  () =>
    !!data.value &&
    c.state.mcp.renaming.has(`${data.value.kind}:${data.value.id}`),
);
</script>
<template>
  <ToolDetails v-if="kind === 'toolDetails'" />
  <ToolEditor v-else-if="kind?.startsWith('toolCenter')" />
  <ModalShell
    v-else-if="kind"
    :title="title"
    :busy="
      c.state.shutdownBusy || renameBusy === true || c.state.mcp.policySaving
    "
    @close="c.closeModal()"
  >
    <template v-if="kind === 'mcpCreate'"
      ><form @submit="c.createMcpSetup">
        <div class="modal-body">
          <p>
            为一台客户端生成一次性设置，设置代码和文件含有凭据，请妥善传递。
          </p>
          <div class="input-group">
            <label class="input-label" for="mcp-setup-name"
              >客户端名称（可选）</label
            ><input
              id="mcp-setup-name"
              v-model="c.state.mcp.setupName"
              class="input"
              :disabled="c.state.mcp.creating"
            />
          </div>
          <label class="input-label" for="mcp-setup-expiry"
            >首次兑换有效期</label
          ><select
            id="mcp-setup-expiry"
            v-model="c.state.mcp.setupExpiry"
            class="input"
            :disabled="c.state.mcp.creating"
          >
            <option :value="1">1 小时</option>
            <option :value="24">24 小时（默认）</option>
            <option :value="168">7 天</option>
          </select>
          <p class="mcp-note">
            不配对
            Agent，不授予完全控制；同一安装实例可续接，不能注册第二台客户端。
          </p>
          <p v-if="c.state.mcp.modalError" class="mcp-error" role="alert">
            {{ c.state.mcp.modalError }}
          </p>
        </div>
        <footer class="modal-footer">
          <button
            type="button"
            class="btn btn-secondary"
            @click="c.closeModal()"
          >
            取消</button
          ><button
            class="btn btn-primary"
            :disabled="c.state.mcp.createAttempted"
          >
            {{
              c.state.mcp.creating
                ? "正在生成…"
                : c.state.mcp.createAttempted
                  ? "请关闭后核对记录"
                  : "生成设置"
            }}
          </button>
        </footer>
      </form></template
    >
    <template v-else-if="kind === 'mcpFresh' && c.state.mcp.freshSetup"
      ><div class="modal-body">
        <div class="mcp-notice">
          <strong>仅此一次显示</strong>
          <p>关闭弹窗、离开页面或退出登录后不可重新查看。</p>
        </div>
        <label for="mcp-setup-code" class="input-label">一次性设置代码</label
        ><textarea
          id="mcp-setup-code"
          class="input font-mono"
          readonly
          autocomplete="off"
          rows="6"
          :value="c.state.mcp.freshSetup.setup_code"
        />
        <p>
          首次兑换截止：{{
            c.formatApiDate(c.state.mcp.freshSetup.setup.expires_at)
          }}
        </p>
      </div>
      <footer class="modal-footer">
        <button class="btn btn-secondary" @click="c.downloadMcpSetup()">
          下载 .remoteops-setup</button
        ><button class="btn btn-primary" @click="c.copyMcpSetupCode()">
          复制设置代码</button
        ><button class="btn btn-secondary" @click="c.closeModal()">
          完成并清除
        </button>
      </footer></template
    >
    <template v-else-if="kind === 'mcpRename' && data"
      ><form @submit="c.renameMcpClient">
        <div class="modal-body">
          <label for="mcp-client-name" class="input-label"
            >客户端名称（可选）</label
          ><input
            id="mcp-client-name"
            v-model="data.name"
            class="input"
            :disabled="renameBusy"
          />
          <p class="mcp-note">
            最多 256 个 UTF-8 字节，留空显示为未命名客户端；不改变已有凭据。
          </p>
          <p v-if="c.state.mcp.modalError" role="alert" class="mcp-error">
            {{ c.state.mcp.modalError }}
          </p>
        </div>
        <footer class="modal-footer">
          <button
            type="button"
            class="btn btn-secondary"
            @click="c.closeModal()"
          >
            取消</button
          ><button class="btn btn-primary" :disabled="renameBusy">
            {{ renameBusy ? "正在保存…" : "保存名称" }}
          </button>
        </footer>
      </form></template
    >
    <template v-else-if="kind === 'mcpRevoke' && data"
      ><div class="modal-body">
        <strong>{{ data.name }}</strong>
        <p class="font-mono text-break">{{ data.id }}</p>
        <p>
          {{
            data.kind === "setups"
              ? "撤销后不能首次兑换或用该设置续接。已注册客户端凭据需单独撤销。"
              : "撤销后该安装实例失去访问权限，恢复接入需要重新登记。"
          }}
        </p>
        <p v-if="c.state.mcp.modalError" role="alert" class="mcp-error">
          {{ c.state.mcp.modalError }}
        </p>
      </div>
      <footer class="modal-footer">
        <button class="btn btn-secondary" @click="c.closeModal()">取消</button
        ><button
          class="btn btn-danger"
          :disabled="c.state.mcp.revoking.has(data.kind + ':' + data.id)"
          @click="c.confirmMcpRevoke()"
        >
          确认撤销
        </button>
      </footer></template
    >
    <template v-else-if="kind === 'mcpLegacyDisable'"
      ><div class="modal-body">
        <p>
          现有旧 Token MCP 连接将断开并无法重连。独立客户端凭据和人工 Controller
          不受影响。
        </p>
      </div>
      <footer class="modal-footer">
        <button class="btn btn-secondary" @click="c.closeModal()">取消</button
        ><button
          class="btn btn-danger"
          :disabled="c.state.mcp.policySaving"
          @click="c.saveMcpAccessPolicy(false)"
        >
          确认关闭旧 Token 接入
        </button>
      </footer></template
    >
    <template v-else-if="kind === 'terminateSession'"
      ><div class="modal-body">
        <p>关闭后 Controller 将失去控制权，未完成的交互指令会中断。</p>
        <strong>{{ data?.agentName }}</strong>
        <p class="font-mono text-break">{{ data?.id }}</p>
      </div>
      <footer class="modal-footer">
        <button class="btn btn-secondary" @click="c.closeModal()">取消</button
        ><button class="btn btn-danger" @click="c.confirmTerminateSession()">
          确认关闭会话
        </button>
      </footer></template
    >
    <template v-else-if="kind === 'emergencyStop'"
      ><div class="modal-body">
        <p class="mcp-error">立即中断所有正在执行的远程操作并断开会话。</p>
        <strong>{{ data?.agentName }}</strong>
        <p class="font-mono text-break">{{ data?.id }}</p>
        <label for="emergency-stop-confirmation" class="input-label"
          >请输入大写 STOP 以确认</label
        ><input
          id="emergency-stop-confirmation"
          v-model="stop"
          class="input font-mono"
          autocomplete="off"
        />
      </div>
      <footer class="modal-footer">
        <button class="btn btn-secondary" @click="c.closeModal()">取消</button
        ><button
          class="btn btn-danger"
          :disabled="stop.trim() !== 'STOP'"
          @click="c.confirmEmergencyStop()"
        >
          确认紧急停止
        </button>
      </footer></template
    >
    <template v-else-if="kind === 'shutdownAgents' || kind === 'shutdownAgent'"
      ><div class="modal-body">
        <p>
          结束远程任务并退出以下 {{ data?.targets?.length || 0 }} 个
          Agent，不会关闭服务器。
        </p>
        <p v-if="data?.all">包括当前筛选之外的全部可关闭在线节点。</p>
        <ul>
          <li v-for="target in data?.targets" :key="target.id">
            {{ target.hostname }}
          </li>
        </ul>
        <ul v-if="data?.results">
          <li v-for="result in data.results" :key="result.id">
            {{ result.hostname }}：{{ result.message }}
          </li>
        </ul>
        <p v-if="data?.refreshError" class="mcp-error">
          结果已保留；节点列表刷新失败，请稍后重新读取。
        </p>
      </div>
      <footer class="modal-footer">
        <button
          class="btn btn-secondary"
          :disabled="c.state.shutdownBusy"
          @click="c.closeModal()"
        >
          {{ data?.results ? "完成" : "取消" }}</button
        ><button
          v-if="!data?.results"
          class="btn btn-danger"
          :disabled="c.state.shutdownBusy"
          @click="c.confirmShutdownAgent()"
        >
          确认关闭 Agent
        </button>
      </footer></template
    >
    <template v-else-if="kind === 'purgeClosedSessions'"
      ><div class="modal-body">
        <p>只清除节点已离线的已关闭会话记录，保留活跃连接。</p>
      </div>
      <footer class="modal-footer">
        <button class="btn btn-secondary" @click="c.closeModal()">取消</button
        ><button class="btn btn-danger" @click="c.confirmPurgeClosedSessions()">
          确认清除
        </button>
      </footer></template
    >
  </ModalShell>
</template>
