<script setup lang="ts">
import { computed, watch } from 'vue'
import { useRoute } from 'vue-router'
import { publishChats } from '../chat-list'
import Icon from '../components/Icon.vue'
import RunWorkspace from '../components/RunWorkspace.vue'
import { Plus } from '../icons'
import { modifier } from '../shortcuts'
import { filRunPath } from '../signal'
import { useLiveRun } from '../use-live-run'

// Home: the Fil beside its reading pane on wide screens, the Fil alone on phones. A mission opened
// from the Fil reads in that pane; on narrow screens it takes the page, as a conversation does.
const route = useRoute()
const runId = computed(() => route.params.id as string | undefined)
const live = useLiveRun(() => '/chats/stream')
watch(live.snapshot, (value) => {
  if (value?.chats)
    publishChats(value.chats)
})
</script>

<template>
  <div class="fil-page flex h-full min-h-0 min-w-0 flex-1" :class="{ 'tablet:hidden': !runId }">
    <section
      v-if="runId"
      class="fil-run flex min-h-0 min-w-0 flex-1 flex-col px-9 pt-8 pb-7 tablet:px-6 phone:px-3 phone:pt-[max(14px,env(safe-area-inset-top))] phone:pb-3 [&_.back-link]:hidden tablet:[&_.back-link]:inline-flex"
      aria-label="Reading pane"
    >
      <RunWorkspace
        :key="runId"
        :run-id="runId"
        :run-path="filRunPath"
        :back="{ to: '/', label: 'Back to the Fil' }"
        mission
      />
    </section>
    <section v-else class="flex min-w-0 flex-1 flex-col items-center justify-center gap-5 px-8 text-center tablet:hidden" aria-label="Reading pane">
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
