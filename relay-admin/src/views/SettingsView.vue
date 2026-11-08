<script setup lang="ts">
import { ref } from "vue";
import { changePassword } from "../services/operations";
const current = ref(""),
  password = ref(""),
  confirm = ref(""),
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
        <p class="page-desc">修改密码后管理会话将失效，需要重新登录。</p>
      </div>
    </header>
    <form class="card settings-form" @submit.prevent="save">
      <h2 class="section-title">修改管理员密码</h2>
      <div class="input-group">
        <label for="current-admin-password" class="input-label">当前密码</label
        ><input
          id="current-admin-password"
          v-model="current"
          class="input"
          type="password"
          autocomplete="current-password"
          required
        />
      </div>
      <div class="input-group">
        <label for="new-admin-password" class="input-label"
          >新密码（至少 12 字符）</label
        ><input
          id="new-admin-password"
          v-model="password"
          class="input"
          type="password"
          autocomplete="new-password"
          minlength="12"
          required
        />
      </div>
      <div class="input-group">
        <label for="confirm-admin-password" class="input-label"
          >确认新密码</label
        ><input
          id="confirm-admin-password"
          v-model="confirm"
          class="input"
          type="password"
          autocomplete="new-password"
          required
        />
      </div>
      <p v-if="error" class="mcp-error" role="alert">{{ error }}</p>
      <button class="btn btn-primary" :disabled="busy">
        {{ busy ? "正在保存…" : "确认修改密码" }}
      </button>
    </form>
  </div>
</template>
