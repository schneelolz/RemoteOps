<script setup lang="ts">
import * as c from "../services/operations";
import EmptyState from "../components/EmptyState.vue";
const labels: Record<string, string> = {
  pending: "待兑换",
  redeemed: "已兑换",
  expired: "已过期",
  revoked: "已撤销",
};
const fields = [
  ["relay", "Relay 地址", "tls://host:7443"],
  ["server_name", "TLS 服务名", "证书对应域名"],
  ["enrollment_url", "登记地址", "https://host/api/mcp/enroll"],
];
</script>
<template>
  <div class="content-container">
    <header class="page-header">
      <div>
        <h1 class="page-title" tabindex="-1">MCP 客户端接入</h1>
        <p class="page-desc">
          配置接入地址，为客户端生成一次性设置并管理独立凭据。
        </p>
      </div>
      <button
        class="btn btn-primary"
        :disabled="
          !c.state.mcp.settings ||
          c.state.mcp.settingsDirty ||
          c.state.mcp.loading ||
          c.state.mcp.saving
        "
        @click="c.openMcpSetup()"
      >
        新建一次性设置
      </button>
    </header>
    <p v-if="c.state.mcp.error" class="mcp-error" role="alert">
      {{ c.state.mcp.error }}
    </p>
    <section class="card">
      <h2 class="section-title">旧 Token 客户端接入</h2>
      <p class="section-desc">
        关闭后旧共享 Token 的 MCP 连接将断开；独立客户端凭据和人工 Controller
        不受影响。
      </p>
      <label class="tool-permission"
        ><input
          type="checkbox"
          :checked="c.state.mcp.accessPolicy?.allow_legacy_token === true"
          :disabled="!c.state.mcp.policyLoaded || c.state.mcp.policySaving"
          @change="
            c.requestMcpAccessPolicy(
              ($event.target as HTMLInputElement).checked,
            )
          "
        />允许旧 Token 接入</label
      >
      <p v-if="c.state.mcp.policyError" class="mcp-error">
        {{ c.state.mcp.policyError }}
      </p>
    </section>
    <form
      v-if="c.state.mcp.settingsDraft"
      class="card"
      @submit="c.saveMcpSettings"
    >
      <h2 class="section-title">接入配置</h2>
      <fieldset class="tools-fields" :disabled="c.state.mcp.saving">
        <div class="tools-form-grid">
          <div
            v-for="field in fields"
            :key="field[0]"
            class="input-group tools-wide"
          >
            <label class="input-label" :for="'mcp-' + field[0]">{{
              field[1]
            }}</label
            ><input
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
        <details>
          <summary>公开 CA 证书（可选）</summary>
          <div
            v-for="field in [
              ['relay_ca_pem', 'Relay CA 证书'],
              ['enrollment_ca_pem', '登记服务 CA 证书'],
            ]"
            :key="field[0]"
            class="input-group"
          >
            <label class="input-label" :for="'mcp-' + field[0]">{{
              field[1]
            }}</label
            ><textarea
              :id="'mcp-' + field[0]"
              class="input font-mono"
              rows="5"
              :value="c.state.mcp.settingsDraft[field[0]]"
              @input="
                c.updateMcpSetting(
                  field[0],
                  ($event.target as HTMLTextAreaElement).value,
                )
              "
            />
          </div>
        </details>
        <div class="mcp-form-actions">
          <p class="mcp-note">
            {{
              c.state.mcp.settingsDirty
                ? "有未保存的修改，保存后才能生成设置。"
                : "接入地址与公开证书保存到 Relay。"
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
          <p class="section-desc">
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
                <strong>{{ item.client_name || "未命名客户端" }}</strong>
                <div class="font-mono table-secondary-text">
                  {{ item.grant_id }}
                </div>
              </td>
              <td>{{ labels[item.state] || "未知" }}</td>
              <td>{{ c.formatApiDate(item.created_at) }}</td>
              <td>{{ c.formatApiDate(item.expires_at) }}</td>
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
                    修改名称</button
                  ><button
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
        description="先保存接入配置，再生成一次性设置。"
      />
    </section>
    <section class="table-container" aria-labelledby="mcp-clients-title">
      <div class="table-header-bar">
        <div>
          <h2 class="section-title" id="mcp-clients-title">已注册客户端</h2>
          <p class="section-desc">
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
                <strong>{{ item.client_name || "未命名客户端" }}</strong>
                <div class="font-mono table-secondary-text">
                  {{ item.installation_id }}
                </div>
              </td>
              <td>{{ item.revoked ? "已撤销" : "凭据有效" }}</td>
              <td>
                <div>
                  {{ item.tool_management ? "普通使用＋工具管理" : "普通使用" }}
                </div>
                <label class="tool-permission"
                  ><input
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
                  />{{ item.tool_management ? "已授予" : "未授予" }}</label
                >
              </td>
              <td>{{ c.formatApiDate(item.created_at) }}</td>
              <td>
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
                    修改名称</button
                  ><button
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
      <EmptyState v-else title="暂无已注册客户端" />
    </section>
  </div>
</template>
