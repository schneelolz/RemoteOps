<script setup lang="ts">
defineProps<{ title: string; description?: string }>();
</script>
<template>
  <div class="empty-state">
    <h3 class="empty-state-title">{{ title }}</h3>
    <p v-if="description" class="empty-state-desc">{{ description }}</p>
    <slot />
  </div>
</template>
