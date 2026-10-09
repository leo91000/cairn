<script setup lang="ts">
import type { Agent } from '../../../../packages/contracts/contracts'
import { computed, onMounted, ref } from 'vue'
import { updateAgentPortrait } from '../agent-avatars'
import { api, notify, state } from '../api'
import { LoaderCircle, RefreshCw, Sparkles } from '../icons'
import AgentAvatar from './AgentAvatar.vue'
import Icon from './Icon.vue'
import UiButton from './UiButton.vue'

const props = defineProps<{ agentId: string | null, name: string, disabled?: boolean }>()
const agent = computed(() => state.agents.find(item => item.id === props.agentId))
const generating = computed(() => agent.value?.avatar?.status === 'generating')
const configured = ref<boolean | null>(null)
const busy = ref(false)
const error = ref('')
const fileInput = ref<HTMLInputElement>()
onMounted(async () => {
  try {
    configured.value = (await api<{ configured: boolean }>('/agent-avatars')).configured
  }
  catch (e) { error.value = (e as Error).message }
})

async function generate() {
  if (!props.agentId || busy.value || generating.value)
    return
  busy.value = true
  error.value = ''
  try {
    updateAgentPortrait(await api<Agent>(`/agents/${props.agentId}/avatar/generate`, { method: 'POST' }))
  }
  catch (e) { error.value = (e as Error).message }
  finally { busy.value = false }
}

async function upload(event: Event) {
  const input = event.target as HTMLInputElement
  const file = input.files?.[0]
  input.value = ''
  if (!file || !props.agentId)
    return
  error.value = ''
  if (file.size > 5 * 1024 * 1024) {
    error.value = 'Choose an image smaller than 5 MB.'
    return
  }

  busy.value = true
  try {
    updateAgentPortrait(await api<Agent>(`/agents/${props.agentId}/avatar`, {
      method: 'PUT',
      body: file,
      headers: { 'Content-Type': file.type || 'application/octet-stream' },
    }))
    notify('Portrait updated')
  }
  catch (e) { error.value = (e as Error).message }
  finally { busy.value = false }
}
</script>

<template>
  <section aria-label="Agent portrait" class="col-span-2 phone:col-span-1 flex items-start gap-4 rounded-xl border border-line bg-hover/50 p-4">
    <AgentAvatar :name="name || '?'" :identity="agentId || ''" :size="64" />
    <div class="min-w-0 flex-1">
      <h3 class="text-sm font-semibold">
        Portrait
      </h3>
      <p v-if="generating" role="status" class="mt-1 flex items-center gap-1.5 text-xs text-muted">
        <Icon :name="LoaderCircle" :size="13" class="animate-spin motion-reduce:animate-none" />Creating a portrait… You can keep working.
      </p>
      <p v-else-if="!agentId" class="mt-1 text-xs text-muted">
        {{ configured ? 'A unique illustrated portrait will be created from the name and description after you save.' : 'Save your agent to upload a portrait.' }}
      </p>
      <p v-else class="mt-1 text-xs text-muted">
        {{ configured ? 'Generation uses your connected Codex subscription quota. Changes are saved immediately.' : 'Upload an image to give your agent a face. Changes here are saved immediately.' }}
      </p>
      <div v-if="agentId" class="mt-3 flex flex-wrap gap-2">
        <UiButton
          v-if="configured"
          size="small"
          :disabled="disabled || busy || generating"
          @click="generate"
        >
          <Icon :name="agent?.avatar?.url ? RefreshCw : Sparkles" :size="14" />{{ agent?.avatar?.url ? 'Regenerate portrait' : 'Generate portrait' }}
        </UiButton>
        <UiButton size="small" :disabled="disabled || busy" @click="fileInput?.click()">
          {{ busy ? 'Please wait…' : 'Upload image' }}
        </UiButton>
        <input
          ref="fileInput"
          class="hidden"
          type="file"
          accept="image/png,image/jpeg,image/webp"
          aria-label="Upload agent portrait"
          :disabled="disabled || busy"
          @change="upload"
        >
      </div>
      <p v-if="agentId" class="mt-2 text-xs text-muted">
        PNG, JPEG or WebP · up to 5 MB · cropped to a square.
      </p>
      <p v-if="configured === false" class="mt-2 text-xs text-muted">
        Connect and enable a Codex account in Connections to generate portraits.
      </p>
      <p v-if="error || agent?.avatar?.error" role="alert" class="mt-2 text-xs text-coral">
        {{ error || agent?.avatar?.error }}
      </p>
    </div>
  </section>
</template>
