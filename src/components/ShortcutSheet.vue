<script setup lang="ts">
import { shortcutGroups } from '../shortcuts'
import Modal from './Modal.vue'

defineEmits<{ close: [] }>()
</script>

<template>
  <Modal title="Keyboard shortcuts" wide @close="$emit('close')">
    <div class="grid gap-x-8 gap-y-6 px-6.5 py-6 sm:grid-cols-2 phone:p-5">
      <section
        v-for="(group, index) in shortcutGroups"
        :key="group.title"
        class="shortcut-group"
        :style="{ animationDelay: `${index * 50}ms` }"
      >
        <h3 class="eyebrow mb-2">
          {{ group.title }}
        </h3>
        <dl class="m-0">
          <div v-for="item in group.items" :key="item.label" class="flex items-center justify-between gap-4 border-b border-line/60 py-2 text-[13px] last:border-0">
            <dt class="text-ink">
              {{ item.label }}
            </dt>
            <dd class="m-0 flex shrink-0 gap-1">
              <kbd v-for="key in item.keys" :key="key" class="keycap">{{ key }}</kbd>
            </dd>
          </div>
        </dl>
      </section>
    </div>
  </Modal>
</template>

<style scoped>
.shortcut-group { animation: group-in 0.32s var(--ease-signal) both; }
@keyframes group-in { from { opacity: 0; transform: translateY(6px); } }
@media (prefers-reduced-motion: reduce) { .shortcut-group { animation: none; } }
</style>
