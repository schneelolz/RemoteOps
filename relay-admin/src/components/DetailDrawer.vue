<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, watch } from "vue";
import * as c from "../services/operations";
const item = computed(
  () => c.state.activeDrawerAgent || c.state.activeDrawerSession,
);
const isAgent = computed(() => !!c.state.activeDrawerAgent);
const fields = computed(() =>
  item.value
    ? Object.entries(item.value).filter(([key]) => Object.hasOwn(labels, key))
    : [],
);
const labels: Record<string, string> = {
  hostname: "计算机名",
  agentName: "Agent 名称",
  macAddress: "MAC 地址",
  os: "操作系统",
  permissionLabel: "Agent 权限",
  relayPermissionLabel: "Relay 权限",
  mcpControlLabel: "MCP 模式",
  sessionId: "会话 ID",
  heartbeat: "最后心跳",
  lease: "租约",
  ready: "就绪状态",
  everPaired: "配对记录",
  controllerHostname: "客户端名称",
  controllerMacAddress: "客户端 MAC 地址",
  agentMacAddress: "Agent MAC 地址",
  controllerInstanceId: "控制器 ID",
  controllerType: "控制器类型",
  controllerName: "控制器名称",
  humanController: "人工 Controller",
  ownerUuid: "Owner 身份",
  role: "会话角色",
  connectTime: "连接时间",
  leaseExpire: "会话租约到期",
  leaseExpiresAt: "节点租约到期",
  codeConfigured: "已配置控制码",
  isClosed: "会话已关闭",
  agentStatus: "Agent 状态",
  supportsAgentShutdown: "支持关闭 Agent",
  agentGeneration: "Agent 连接代次",
  pendingApprovals: "待审批",
  inflightRequests: "执行中请求",
  agentId: "Agent ID",
  lastHeartbeat: "最后心跳",
  generation: "连接代次",
  status: "连接状态",
};
let previous: HTMLElement | null = null;
watch(item, async (value) => {
  if (value) {
    previous = document.activeElement as HTMLElement;
    await nextTick();
    document.querySelector<HTMLElement>("#drawer-panel button")?.focus();
  } else if (previous?.isConnected) previous.focus();
});
function keys(event: KeyboardEvent) {
  if (!item.value || c.state.activeModal) return;
  if (event.key === "Escape") {
    event.preventDefault();
    c.closeDrawer();
  }
  if (event.key === "Tab") {
    const fields = [
      ...(document.querySelectorAll<HTMLElement>(
        "#drawer-panel button:not(:disabled),#drawer-panel a",
      ) || []),
    ];
    const first = fields[0],
      last = fields.at(-1);
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last?.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first?.focus();
    }
  }
}
document.addEventListener("keydown", keys);
onBeforeUnmount(() => document.removeEventListener("keydown", keys));
</script>
<template>
  <Teleport to="body"
    ><div
      v-if="item"
      class="drawer-backdrop open"
      id="drawer-backdrop"
      @click.self="c.closeDrawer()"
    >
      <section
        class="drawer-panel open"
        id="drawer-panel"
        role="dialog"
        aria-modal="true"
        aria-labelledby="drawer-title"
      >
        <header class="drawer-header">
          <div>
            <h2 id="drawer-title">
              {{ isAgent ? "Agent 节点详情" : "会话详情" }}
            </h2>
            <p class="font-mono text-break">{{ item.id }}</p>
          </div>
          <button
            class="btn btn-ghost"
            aria-label="关闭详情"
            @click="c.closeDrawer()"
          >
            ✕
          </button>
        </header>
        <div class="drawer-body">
          <dl class="tools-facts">
            <template v-for="[key, value] in fields" :key="key"
              ><dt>{{ labels[key] || key }}</dt>
              <dd>
                {{
                  typeof value === "boolean"
                    ? value
                      ? "是"
                      : "否"
                    : (value ?? "-")
                }}
              </dd></template
            >
          </dl>
          <div class="action-row">
            <button
              class="btn btn-secondary"
              @click="
                c.copyToClipboard(item.id, isAgent ? 'Agent ID' : 'Session ID')
              "
            >
              复制标识</button
            ><template v-if="!isAgent && !item.isClosed"
              ><button
                class="btn btn-secondary"
                @click="
                  c.openModal('terminateSession', {
                    id: item.id,
                    agentName: item.agentName,
                  })
                "
              >
                关闭会话</button
              ><button
                class="btn btn-danger"
                @click="
                  c.openModal('emergencyStop', {
                    id: item.id,
                    agentName: item.agentName,
                  })
                "
              >
                紧急停止
              </button></template
            ><button
              v-if="isAgent && c.canShutdownAgent(item)"
              class="btn btn-danger"
              @click="
                c.openModal('shutdownAgent', {
                  id: item.id,
                  generation: item.generation,
                })
              "
            >
              关闭 Agent
            </button>
          </div>
        </div>
      </section>
    </div></Teleport
  >
</template>
