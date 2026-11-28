<script setup lang="ts">
import { computed, ref } from "vue";
import McpDeviceGrants from "../components/McpDeviceGrants.vue";
import { Plus, ShieldAlert, KeyRound } from "@lucide/vue";
import * as c from "../services/operations";
import EmptyState from "../components/EmptyState.vue";

const setupStateMeta: Record<string, { label: string; cls: string }> = {
  pending: { label: "待兑换", cls: "badge-info" },
  redeemed: { label: "已兑换", cls: "badge-online" },
  expired: { label: "已过期", cls: "badge-degraded" },
  revoked: { label: "已撤销", cls: "badge-danger" },
};

const selectedInstallation = ref("");
const grantClient = computed(() =>
  c.state.mcp.clients.find(
    (item) =>
      item.installation_id === selectedInstallation.value && !item.revoked,
  ),
);
const fields = [
  ["relay", "Relay 地址", "host:7443"],
  ["server_name", "TLS 服务名", "证书对应域名"],
  ["enrollment_url", "登记地址", "https://host/api/mcp/enroll"],
] as const;
</script>
<template>
  <div class="content-container mcp-onboarding">
    <header class="page-header">
      <div>
        <h1 class="page-title" tabindex="-1">MCP 客户端接入</h1>
        <p class="page-desc">
          配置客户端接入地址与公开信任证书，生成一次性设置并管理独立客户端凭据。
        </p>
      </div>
      <div class="page-header-actions">
        <button
          id="mcp-new-setup"
          class="btn btn-primary"
          :disabled="
            !c.state.mcp.settings ||
            c.state.mcp.settingsDirty ||
            c.state.mcp.loading ||
            c.state.mcp.saving
          "
          @click="c.openMcpSetup()"
        >
          <Plus :size="16" aria-hidden="true" />
          <span>新建一次性设置</span>
        </button>
      </div>
    </header>

    <p v-if="c.state.mcp.error" class="mcp-error" role="alert">
      {{ c.state.mcp.error }}
    </p>

    <section class="card tools-policy">
      <div>
        <div class="card-title-row">
          <h2 class="section-title">
            <ShieldAlert :size="18" aria-hidden="true" />
            <span>旧 Token 客户端接入</span>
          </h2>
          <span
            v-if="c.state.mcp.policyLoaded"
            class="badge"
            :class="
              c.state.mcp.accessPolicy?.allow_legacy_token
                ? 'badge-degraded'
                : 'badge-online'
            "
          >
            {{
              c.state.mcp.accessPolicy?.allow_legacy_token
                ? "兼容模式开启"
                : "仅独立凭据"
            }}
          </span>
        </div>
        <p class="section-desc">
          关闭后旧共享 Token 的 MCP 连接将断开；独立客户端凭据和人工 Controller
          不受影响。
        </p>
        <p v-if="c.state.mcp.policyError" class="mcp-error">
          {{ c.state.mcp.policyError }}
        </p>
      </div>
      <label class="tool-permission permission-pill">
        <input
          type="checkbox"
          :checked="c.state.mcp.accessPolicy?.allow_legacy_token === true"
          :disabled="!c.state.mcp.policyLoaded || c.state.mcp.policySaving"
          @change="
            c.requestMcpAccessPolicy(
              ($event.target as HTMLInputElement).checked,
            )
          "
        />
        <span>允许旧 Token 接入</span>
      </label>
    </section>

    <form
      v-if="c.state.mcp.settingsDraft"
      class="card"
      @submit="c.saveMcpSettings"
    >
      <div class="card-header-row">
        <div>
          <h2 class="section-title">
            <KeyRound :size="18" aria-hidden="true" />
            <span>接入配置</span>
          </h2>
          <p class="section-desc">
            明确填写客户端实际可访问的地址与 TLS
            服务名，保存后即可签发一次性设置。
          </p>
        </div>
        <span v-if="c.state.mcp.settingsDirty" class="badge badge-degraded"
          >有未保存修改</span
        >
      </div>

      <fieldset class="tools-fields" :disabled="c.state.mcp.saving">
        <div class="mcp-endpoint-grid">
          <div
            v-for="(field, index) in fields"
            :key="field[0]"
            class="input-group"
            :class="{ 'mcp-full-width': index === 2 }"
          >
            <label class="input-label" :for="'mcp-' + field[0]">{{
              field[1]
            }}</label>
            <input
              :id="'mcp-' + field[0]"
              class="input font-mono"
              :placeholder="field[2]"
              :value="c.state.mcp.settingsDraft[field[0]]"
              @input="
                c.updateMcpSetting(
                  field[0],
                  ($event.target as HTMLInputElement).value,
                )
              "
            />
          </div>
        </div>

        <details class="mcp-trust-details">
          <summary>公开 CA 证书（自签名或私有 CA 场景可选）</summary>
          <div class="mcp-ca-grid">
            <div
              v-for="field in [
                ['relay_ca_pem', 'Relay CA 证书 (PEM)'],
                ['enrollment_ca_pem', '登记服务 CA 证书 (PEM)'],
              ]"
              :key="field[0]"
              class="input-group"
            >
              <label class="input-label" :for="'mcp-' + field[0]">{{
                field[1]
              }}</label>
              <textarea
                :id="'mcp-' + field[0]"
                class="input font-mono mcp-pem"
                rows="5"
                placeholder="-----BEGIN CERTIFICATE-----&#10;…&#10;-----END CERTIFICATE-----"
                :value="c.state.mcp.settingsDraft[field[0]]"
                @input="
                  c.updateMcpSetting(
                    field[0],
                    ($event.target as HTMLTextAreaElement).value,
                  )
                "
              />
            </div>
          </div>
        </details>

        <div class="mcp-form-actions">
          <p class="mcp-note">
            {{
              c.state.mcp.settingsDirty
                ? "有未保存的修改，保存后才能生成一次性设置。"
                : "接入地址与公开证书保存到 Relay，不包含私钥。"
            }}
          </p>
          <button type="submit" class="btn btn-primary">
            {{ c.state.mcp.saving ? "正在保存…" : "保存接入配置" }}
          </button>
        </div>
      </fieldset>
    </form>

    <section class="table-container">
      <div class="table-header-bar">
        <div>
          <h2 class="section-title">一次性设置记录</h2>
          <p class="table-helper-text">
            有效期仅限制首次兑换，同一安装实例可续接。历史设置代码不会再次显示。
          </p>
        </div>
      </div>
      <div v-if="c.state.mcp.setups.length" class="table-wrapper">
        <table class="ops-table mcp-table">
          <thead>
            <tr>
              <th>客户端／设置 ID</th>
              <th>状态</th>
              <th>创建时间</th>
              <th>首次兑换截止</th>
              <th>安装实例</th>
              <th>操作</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="item in c.state.mcp.setups" :key="item.grant_id">
              <td>
                <strong class="table-primary-text">{{
                  item.client_name || "未命名客户端"
                }}</strong>
                <div class="font-mono table-secondary-text">
                  {{ item.grant_id }}
                </div>
              </td>
              <td>
                <span
                  class="badge"
                  :class="setupStateMeta[item.state]?.cls || 'badge-offline'"
                >
                  <span class="badge-dot" aria-hidden="true"></span>
                  {{ setupStateMeta[item.state]?.label || "未知" }}
                </span>
              </td>
              <td class="font-mono">{{ c.formatApiDate(item.created_at) }}</td>
              <td class="font-mono">{{ c.formatApiDate(item.expires_at) }}</td>
              <td class="font-mono">
                {{ item.installation_id || "尚未兑换" }}
              </td>
              <td>
                <div class="action-row">
                  <button
                    class="btn btn-secondary btn-sm"
                    :disabled="
                      c.state.mcp.renaming.has('setups:' + item.grant_id)
                    "
                    @click="c.openMcpRename('setups', item.grant_id)"
                  >
                    修改名称
                  </button>
                  <button
                    class="btn btn-secondary btn-sm"
                    :disabled="
                      item.state === 'revoked' ||
                      c.state.mcp.revoking.has('setups:' + item.grant_id)
                    "
                    @click="c.openMcpRevoke('setups', item.grant_id)"
                  >
                    撤销设置
                  </button>
                </div>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <EmptyState
        v-else
        title="暂无一次性设置"
        description="先保存接入配置，再点击右上角“新建一次性设置”生成。"
      />
    </section>

    <section class="table-container" aria-labelledby="mcp-clients-title">
      <div class="table-header-bar">
        <div>
          <h2 class="section-title" id="mcp-clients-title">已登记客户端</h2>
          <p class="table-helper-text">
            工具管理权与现场完全控制独立，绑定安装身份。共享凭据的宿主共用权限。
          </p>
        </div>
      </div>
      <div v-if="c.state.mcp.clients.length" class="table-wrapper">
        <table class="ops-table mcp-table mcp-clients-table">
          <thead>
            <tr>
              <th>客户端／安装实例</th>
              <th>凭据状态</th>
              <th>使用与管理权限</th>
              <th>注册时间</th>
              <th>最后在线</th>
              <th>操作</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="item in c.state.mcp.clients" :key="item.installation_id">
              <td>
                <strong class="table-primary-text">{{
                  item.client_name || "未命名客户端"
                }}</strong>
                <div class="font-mono table-secondary-text">
                  {{ item.installation_id }}
                </div>
              </td>
              <td>
                <span
                  class="badge"
                  :class="item.revoked ? 'badge-danger' : 'badge-online'"
                >
                  <span class="badge-dot" aria-hidden="true"></span>
                  {{ item.revoked ? "已撤销" : "凭据有效" }}
                </span>
              </td>
              <td>
                <div class="table-primary-text">
                  {{ item.tool_management ? "普通使用＋工具管理" : "普通使用" }}
                </div>
                <label class="tool-permission">
                  <input
                    type="checkbox"
                    :aria-label="
                      (item.client_name || '未命名客户端') + '的工具管理权限'
                    "
                    :checked="item.tool_management"
                    :disabled="
                      item.revoked ||
                      c.state.mcp.revoking.has(
                        'permission:' + item.installation_id,
                      )
                    "
                    @change="
                      c.saveToolManagementPermission(
                        item.installation_id,
                        ($event.target as HTMLInputElement).checked,
                      )
                    "
                  />
                  <span>{{ item.tool_management ? "已授予" : "未授予" }}</span>
                </label>
              </td>
              <td class="font-mono">{{ c.formatApiDate(item.created_at) }}</td>
              <td class="font-mono">
                {{
                  item.last_seen ? c.formatApiDate(item.last_seen) : "尚未连接"
                }}
              </td>
              <td>
                <div class="action-row">
                  <button
                    class="btn btn-secondary btn-sm"
                    :disabled="
                      c.state.mcp.renaming.has(
                        'clients:' + item.installation_id,
                      )
                    "
                    @click="c.openMcpRename('clients', item.installation_id)"
                  >
                    修改名称
                  </button>
                  <button
                    class="btn btn-secondary btn-sm"
                    :disabled="item.revoked || c.state.demoMode"
                    @click="selectedInstallation = item.installation_id"
                  >
                    设备授权
                  </button>
                  <button
                    class="btn btn-secondary btn-sm"
                    :disabled="
                      item.revoked ||
                      c.state.mcp.revoking.has(
                        'clients:' + item.installation_id,
                      )
                    "
                    @click="c.openMcpRevoke('clients', item.installation_id)"
                  >
                    撤销客户端
                  </button>
                </div>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <EmptyState
        v-else
        title="暂无已登记客户端"
        description="客户端通过一次性设置完成登记后将在此显示。"
      />
    </section>
    <McpDeviceGrants
      v-if="grantClient"
      :key="grantClient.installation_id"
      :installation="grantClient.installation_id"
      :name="grantClient.client_name || '未命名客户端'"
      @close="selectedInstallation = ''"
    />
  </div>
</template>
