<script setup lang="ts">
import { nextTick, onBeforeUnmount, onMounted, ref } from "vue";
import { rememberModalTrigger, restoreModalTrigger } from "./modal-focus";
const props = defineProps<{ title: string; wide?: boolean; busy?: boolean }>();
const emit = defineEmits<{ close: [] }>();
const panel = ref<HTMLElement>();
rememberModalTrigger();
function keys(e: KeyboardEvent) {
  if (e.key === "Escape" && !props.busy) {
    e.preventDefault();
    emit("close");
  }
  if (e.key !== "Tab") return;
  const fields = [
    ...(panel.value?.querySelectorAll<HTMLElement>(
      'button:not(:disabled),input:not(:disabled),select:not(:disabled),textarea:not(:disabled),summary,[tabindex="0"]',
    ) || []),
  ].filter((el) => el.getClientRects().length);
  const first = fields[0],
    last = fields.at(-1);
  if (e.shiftKey && document.activeElement === first) {
    e.preventDefault();
    last?.focus();
  } else if (!e.shiftKey && document.activeElement === last) {
    e.preventDefault();
    first?.focus();
  }
}
onMounted(async () => {
  document.addEventListener("keydown", keys);
  await nextTick();
  panel.value?.querySelector<HTMLElement>("button,input")?.focus();
});
onBeforeUnmount(() => {
  document.removeEventListener("keydown", keys);
  void nextTick(restoreModalTrigger);
});
</script>
<template>
  <Teleport to="body"
    ><div
      class="modal-backdrop open"
      id="modal-backdrop"
      @mousedown.self="!busy && emit('close')"
    >
      <section
        ref="panel"
        class="modal"
        :class="{ 'tools-editor': wide }"
        role="dialog"
        aria-modal="true"
        aria-labelledby="dialog-title"
      >
        <header class="modal-header">
          <h2 class="modal-title" id="dialog-title">{{ title }}</h2>
          <button
            type="button"
            class="btn btn-ghost"
            aria-label="关闭弹窗"
            :disabled="busy"
            @click="emit('close')"
          >
            ✕
          </button>
        </header>
        <slot />
      </section></div
  ></Teleport>
</template>
