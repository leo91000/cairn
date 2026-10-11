<script setup lang="ts">
import type { Run } from '../../../../packages/contracts/contracts'
import { computed, inject } from 'vue'
import { useRouter } from 'vue-router'
import { state } from '../api'
import { ChevronRight } from '../icons'
import { connectionTone, installationKey, routeLabel } from '../installation-context'
import Icon from './Icon.vue'
import Modal from './Modal.vue'
import NodeExecution from './NodeExecution.vue'
import VirtualSelect from './VirtualSelect.vue'

// Phones keep the installation, its route and the runner here instead of in permanent bars.
defineProps<{
  run?: Run | null
  agent?: string
  project?: string | null
  returnFocus?: HTMLElement
}>()
const emit = defineEmits<{ close: [] }>()
const router = useRouter()
const installation = inject(installationKey, null)
const current = computed(() => installation?.current.value ?? null)

function select(id: string) {
  if (id !== current.value?.id)
    installation?.select(id)
}

function add() {
  emit('close')
  installation?.add()
}

function openSettings() {
  emit('close')
  void router.push('/settings/installation')
}
</script>

<template>
  <Modal
    title="Connection"
    sheet
    :return-focus="returnFocus"
    @close="emit('close')"
  >
    <div class="px-5 pb-5">
      <dl class="connection-rows m-0 text-sm">
        <div>
          <dt>Installation</dt>
          <dd>
            <VirtualSelect
              v-if="current && current.options.length > 1"
              :model-value="current.id"
              label="Current installation"
              :options="current.options"
              :disabled="current.disabled"
              compact
              hide-label
              variant="ghost"
              action-label="Add an installation"
              class="max-w-60!"
              @update:model-value="select"
              @action="add"
            />
            <span v-else class="truncate font-semibold">{{ current?.name }}</span>
          </dd>
        </div>
        <div>
          <dt>Role</dt>
          <dd>{{ state.installationRole === 'owner' ? 'Owner' : 'Member' }}</dd>
        </div>
        <div>
          <dt>Route</dt>
          <dd class="flex items-center gap-2">
            <span aria-hidden="true" class="size-2 shrink-0 rounded-full" :class="connectionTone(current, state.transportRoute)" />
            <span v-if="current && current.status !== 'Online'" class="text-muted">{{ current.status }} ·</span>
            {{ routeLabel(state.transportRoute) }}
          </dd>
        </div>
        <div v-if="run && state.installationRole === 'owner'" class="connection-runner">
          <dt>Runner</dt>
          <dd>
            <NodeExecution :key="run.id" :run="run" />
          </dd>
        </div>
        <div v-if="agent">
          <dt>Agent · project</dt>
          <dd class="truncate">
            {{ [agent, project].filter(Boolean).join(' · ') }}
          </dd>
        </div>
      </dl>
      <button
        v-if="state.installationRole === 'owner'"
        type="button"
        class="flex min-h-12 w-full items-center justify-between gap-3 text-left text-sm text-muted hover:text-ink"
        @click="openSettings"
      >
        Installation settings<Icon :name="ChevronRight" :size="18" />
      </button>
    </div>
  </Modal>
</template>

<style scoped>
.connection-rows > div {
  display: flex;
  min-height: 52px;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  border-bottom: 1px solid var(--color-line);
}
.connection-rows dt { flex-shrink: 0; color: var(--color-muted); }
.connection-rows dd { margin: 0; min-width: 0; display: flex; justify-content: flex-end; text-align: right; }
/* The runner chip renders nothing while only the master runner exists. */
.connection-runner:not(:has(button)) { display: none; }
</style>
