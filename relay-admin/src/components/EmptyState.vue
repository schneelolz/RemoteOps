<script setup lang="ts">
defineProps<{ title: string; description?: string }>();
</script>
<template>
  <div class="empty-state">
    <h3 class="empty-state-title">{{ title }}</h3>
    <p class="empty-state-desc">{{ description }}</p>
  </div>
</template>
