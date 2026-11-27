<script setup lang="ts">
import { ref } from "vue";
import { Eye, EyeOff, Lock, ShieldCheck } from "@lucide/vue";
import { login, state, setTheme } from "../services/operations";

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
  <div class="login-screen">
    <div class="login-topbar">
      <select
        class="select theme-select"
        aria-label="界面主题"
        :value="state.theme"
        @change="setTheme(($event.target as HTMLSelectElement).value)"
      >
        <option value="system">跟随系统</option>
        <option value="light">亮色主题</option>
        <option value="dark">暗色主题</option>
      </select>
    </div>
    <section class="login-card">
      <div class="login-brand">
        <div class="brand-badge" aria-hidden="true">
          <svg width="28" height="28" viewBox="0 0 32 32" fill="none">
            <path
              d="M10 24V8h7a5 5 0 0 1 0 10h-4l5 6h-3.5l-4.5-6H13v6h-3z"
              fill="currentColor"
            />
          </svg>
        </div>
        <div>
          <div class="login-kicker">REMOTEOPS RELAY</div>
          <h1>管理控制台</h1>
          <p>受控远程运维中继 · 会话、节点与凭据管理</p>
        </div>
      </div>

      <form class="login-form" @submit.prevent="submit">
        <div class="input-group">
          <label for="login-username" class="input-label"
            >管理员账号 (Username)</label
          >
          <input
            id="login-username"
            v-model="username"
            class="input"
            placeholder="请输入管理员账号"
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
              placeholder="请输入管理密码"
              autocomplete="current-password"
              required
            />
            <button
              type="button"
              class="password-toggle-icon"
              :aria-label="visible ? '隐藏密码' : '显示密码'"
              :aria-pressed="visible"
              :title="visible ? '隐藏密码' : '显示密码'"
              @click="visible = !visible"
            >
              <EyeOff v-if="visible" :size="18" aria-hidden="true" />
              <Eye v-else :size="18" aria-hidden="true" />
            </button>
          </div>
        </div>
        <p v-if="error" role="alert" class="mcp-error">{{ error }}</p>
        <button
          type="submit"
          class="btn btn-primary login-submit-btn"
          :disabled="busy"
        >
          <Lock :size="16" aria-hidden="true" />
          <span>{{ busy ? "正在验证…" : "登录管理控制台" }}</span>
        </button>
      </form>

      <div class="login-footer">
        <span class="login-footer-item">
          <ShieldCheck :size="15" aria-hidden="true" />
          <span>会话 Cookie 鉴权，凭据不写入浏览器本地存储</span>
        </span>
      </div>
    </section>
  </div>
</template>
