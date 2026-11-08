<script setup lang="ts">
import { state, toggleVisibleColumn } from "../services/operations";
defineProps<{
  scope: "agents" | "sessions";
  definitions: { key: string; label: string }[];
}>();
</script>
<template>
  <details class="column-picker">
    <summary class="btn btn-secondary">显示列</summary>
    <div class="column-picker-menu">
      <label
        v-for="column in definitions"
        :key="column.key"
        class="column-option"
        ><input
          type="checkbox"
          :checked="state.visibleColumns[scope].includes(column.key)"
          @change="
            toggleVisibleColumn(
              scope,
              column.key,
              ($event.target as HTMLInputElement).checked,
            )
          "
        />{{ column.label }}</label
      >
    </div>
  </details>
</template>
