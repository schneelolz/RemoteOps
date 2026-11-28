<script setup lang="ts">
import { ref } from "vue";
import { Eye, EyeOff, ShieldCheck } from "@lucide/vue";
import { changePassword } from "../services/operations";

const current = ref(""),
  password = ref(""),
  confirm = ref(""),
  showCurrent = ref(false),
  showNew = ref(false),
  showConfirm = ref(false),
  busy = ref(false),
  error = ref("");

async function save() {
  if (busy.value) return;
  error.value = "";
  if (password.value !== confirm.value) {
    error.value = "两次输入的新密码不一致";
    return;
  }
  busy.value = true;
  try {
    await changePassword(current.value, password.value, confirm.value);
    current.value = password.value = confirm.value = "";
    showCurrent.value = showNew.value = showConfirm.value = false;
  } catch {
    error.value = "密码修改失败，请检查当前密码和服务连接。";
  } finally {
    busy.value = false;
  }
}
</script>
<template>
  <div class="content-container">
    <header class="page-header">
      <div>
        <h1 class="page-title" tabindex="-1">安全设置</h1>
        <p class="page-desc">
          管理控制台管理员口令。修改密码后当前管理会话将立即失效，需要使用新密码重新登录。
        </p>
      </div>
    </header>

    <form class="card settings-form" @submit.prevent="save">
      <h2 class="section-title">修改管理员密码</h2>
      <div class="settings-policy">
        <ShieldCheck :size="16" aria-hidden="true" />
        <span
          >新密码长度至少 12 个字符，建议混合使用大小写字母、数字与符号。</span
        >
      </div>

      <div class="input-group">
        <label for="current-admin-password" class="input-label">当前密码</label>
        <div class="password-input">
          <input
            id="current-admin-password"
            v-model="current"
            class="input"
            :type="showCurrent ? 'text' : 'password'"
            placeholder="请输入当前管理员密码"
            autocomplete="current-password"
            required
          />
          <button
            type="button"
            class="password-toggle-icon"
            :aria-label="showCurrent ? '隐藏密码' : '显示密码'"
            :aria-pressed="showCurrent"
            :title="showCurrent ? '隐藏密码' : '显示密码'"
            @click="showCurrent = !showCurrent"
          >
            <EyeOff v-if="showCurrent" :size="18" aria-hidden="true" />
            <Eye v-else :size="18" aria-hidden="true" />
          </button>
        </div>
      </div>

      <div class="input-group">
        <label for="new-admin-password" class="input-label"
          >新密码（至少 12 字符）</label
        >
        <div class="password-input">
          <input
            id="new-admin-password"
            v-model="password"
            class="input"
            :type="showNew ? 'text' : 'password'"
            placeholder="请输入至少 12 位新密码"
            autocomplete="new-password"
            minlength="12"
            required
          />
          <button
            type="button"
            class="password-toggle-icon"
            :aria-label="showNew ? '隐藏密码' : '显示密码'"
            :aria-pressed="showNew"
            :title="showNew ? '隐藏密码' : '显示密码'"
            @click="showNew = !showNew"
          >
            <EyeOff v-if="showNew" :size="18" aria-hidden="true" />
            <Eye v-else :size="18" aria-hidden="true" />
          </button>
        </div>
      </div>

      <div class="input-group">
        <label for="confirm-admin-password" class="input-label"
          >确认新密码</label
        >
        <div class="password-input">
          <input
            id="confirm-admin-password"
            v-model="confirm"
            class="input"
            :type="showConfirm ? 'text' : 'password'"
            placeholder="请再次输入新密码"
            autocomplete="new-password"
            required
          />
          <button
            type="button"
            class="password-toggle-icon"
            :aria-label="showConfirm ? '隐藏密码' : '显示密码'"
            :aria-pressed="showConfirm"
            :title="showConfirm ? '隐藏密码' : '显示密码'"
            @click="showConfirm = !showConfirm"
          >
            <EyeOff v-if="showConfirm" :size="18" aria-hidden="true" />
            <Eye v-else :size="18" aria-hidden="true" />
          </button>
        </div>
      </div>

      <p v-if="error" class="mcp-error" role="alert">{{ error }}</p>

      <div class="settings-form-actions">
        <button type="submit" class="btn btn-primary" :disabled="busy">
          {{ busy ? "正在保存…" : "确认修改密码" }}
        </button>
      </div>
    </form>
  </div>
</template>
