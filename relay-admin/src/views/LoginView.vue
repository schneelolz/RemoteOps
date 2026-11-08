<script setup lang="ts">
import { ref } from "vue";
import { login } from "../services/operations";
const username = ref(""),
  password = ref(""),
  visible = ref(false),
  busy = ref(false),
  error = ref("");
async function submit() {
  if (busy.value) return;
  busy.value = true;
  error.value = "";
  try {
    await login(username.value, password.value);
    password.value = "";
  } catch {
    error.value = "登录失败，请检查管理员账号、密码及服务连接。";
  } finally {
    busy.value = false;
  }
}
</script>
<template>
  <div class="login-wrapper">
    <section class="login-card">
      <div class="brand-badge">R</div>
      <h1 class="login-title">RemoteOps Relay</h1>
      <p class="login-subtitle">登录管理控制台</p>
      <form @submit.prevent="submit">
        <div class="input-group">
          <label for="login-username" class="input-label"
            >管理员账号 (Username)</label
          ><input
            id="login-username"
            v-model="username"
            class="input"
            autocomplete="username"
            required
          />
        </div>
        <div class="input-group">
          <label for="login-password" class="input-label"
            >管理密码 (Password)</label
          >
          <div class="password-input">
            <input
              id="login-password"
              v-model="password"
              class="input"
              :type="visible ? 'text' : 'password'"
              autocomplete="current-password"
              required
            /><button
              type="button"
              class="btn btn-ghost"
              :aria-label="visible ? '隐藏密码' : '显示密码'"
              @click="visible = !visible"
            >
              {{ visible ? "隐藏" : "显示" }}
            </button>
          </div>
        </div>
        <p v-if="error" role="alert" class="mcp-error">{{ error }}</p>
        <button
          type="submit"
          class="btn btn-primary login-submit"
          :disabled="busy"
        >
          {{ busy ? "正在验证…" : "登录管理控制台" }}
        </button>
      </form>
      <p class="mcp-note">使用管理员账号登录，凭据不会保存在浏览器本地存储。</p>
    </section>
  </div>
</template>
