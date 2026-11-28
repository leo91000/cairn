<script setup lang="ts">
import Icon from '../components/Icon.vue'
import { Plus } from '../icons'
import { modifier } from '../shortcuts'
</script>

<template>
  <div class="fil-page flex h-full min-h-0 min-w-0 flex-1 tablet:hidden">
    <section class="flex min-w-0 flex-1 flex-col items-center justify-center gap-5 px-8 text-center" aria-label="Reading pane">
      <p class="font-heading text-2xl font-bold tracking-tight text-ink">
        Pick up where your agents are.
      </p>
      <p class="max-w-sm text-sm text-muted">
        Open anything from the Fil, or start a new conversation.
      </p>
      <RouterLink to="/chats" class="press flex h-11 items-center gap-2 rounded-full bg-accent px-5 text-sm font-semibold text-surface">
        <Icon :name="Plus" :size="18" />New conversation <kbd class="keycap ml-1 border-surface/30! bg-transparent! text-surface!">C</kbd>
      </RouterLink>
      <p class="flex items-center gap-2 text-xs text-subtle">
        <kbd class="keycap">{{ modifier }}</kbd><kbd class="keycap">K</kbd> search · <kbd class="keycap">?</kbd> shortcuts
      </p>
    </section>
  </div>
</template>
