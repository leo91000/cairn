<script setup lang="ts">
import type { Provider } from '../../../../packages/contracts/accounts'
import type { CodingAgentOptions } from '../../../../packages/contracts/generated/typescript/CodingAgentOptions'
import type { ChatQuota } from '../chat-quota'
import {
  computed,
  onBeforeUnmount,
  ref,
  watch,
} from 'vue'
import { providers } from '../../../../packages/contracts/accounts'
import { api } from '../api'
import ChatQuotaCard from './ChatQuotaCard.vue'

const props = defineProps<{
  chatId: string
  quota?: ChatQuota | null
  progress?: { provider: Provider, error: string | null } | null
  canManage: boolean
  readOnly?: boolean
  busy?: boolean
}>()
const emit = defineEmits<{ resume: [], switched: [provider: Provider] }>()

const options = ref<CodingAgentOptions>()
const pending = ref<Provider>()
const error = ref('')
const loadError = ref('')
const sending = ref(false)
const loading = ref(false)
let generation = 0
let lifetime = 0
let timer: ReturnType<typeof setTimeout> | undefined
let controller: AbortController | undefined
const switching = computed(() => pending.value || options.value?.switching?.provider || props.progress?.provider)
const shownQuota = computed(() => props.quota || (switching.value
  ? {
      provider: providers[options.value?.provider || 'codex'].label,
      message: '',
      spend: false,
      reset: null,
      automatic: true,
    }
  : null))

async function load(epoch: number) {
  clearTimeout(timer)
  controller?.abort()
  const request = new AbortController()
  controller = request
  loading.value = !options.value
  let delay = 5000
  try {
    const result = await api<CodingAgentOptions>(`/chats/${props.chatId}/coding-agent`, { signal: request.signal })
    if (epoch !== generation || request.signal.aborted)
      return
    options.value = result
    loadError.value = ''
    if (result.switching && !pending.value)
      pending.value = result.switching.provider
    if (pending.value && !result.switching && !sending.value) {
      if (result.provider === pending.value)
        emit('switched', pending.value)
      pending.value = undefined
    }

    if (result.switching || pending.value)
      delay = 1000
  }
  catch (cause) {
    if (epoch !== generation || request.signal.aborted)
      return
    loadError.value = (cause as Error).message
    delay = 2000
  }
  finally {
    if (epoch === generation && !request.signal.aborted) {
      loading.value = false
      if ((props.quota || switching.value) && !sending.value)
        timer = setTimeout(() => void load(epoch), delay)
    }
  }
}

watch(() => [props.chatId, !!props.quota, props.progress], (_, previous) => {
  const epoch = ++generation
  clearTimeout(timer)
  controller?.abort()
  if (!previous || previous[0] !== props.chatId) {
    lifetime++
    sending.value = false
    options.value = undefined
    pending.value = undefined
    error.value = ''
    loadError.value = ''
  }

  if ((props.quota || props.progress || pending.value) && !sending.value)
    void load(epoch)
}, { immediate: true })
onBeforeUnmount(() => {
  lifetime++
  generation++
  clearTimeout(timer)
  controller?.abort()
})

async function choose(provider: Provider) {
  const selected = options.value
  if (!selected?.runId || !selected.turnId || sending.value || switching.value)
    return
  const chatId = props.chatId
  const operation = lifetime
  clearTimeout(timer)
  controller?.abort()
  loading.value = false
  sending.value = true
  error.value = ''
  pending.value = provider
  try {
    await api(`/chats/${chatId}/coding-agent`, {
      method: 'POST',
      body: JSON.stringify({
        requestId: crypto.randomUUID(),
        runId: selected.runId,
        turnId: selected.turnId,
        provider,
      }),
    })
    if (operation !== lifetime)
      return
    sending.value = false
    await load(generation)
  }
  catch (cause) {
    if (operation !== lifetime)
      return
    pending.value = undefined
    error.value = (cause as Error).message
    sending.value = false
    await load(generation)
  }
  finally {
    if (operation === lifetime)
      sending.value = false
  }
}

async function cancel() {
  const chatId = props.chatId
  const operation = lifetime
  sending.value = true
  clearTimeout(timer)
  controller?.abort()
  try {
    await api(`/chats/${chatId}/stop`, { method: 'POST' })
    if (operation !== lifetime)
      return
    pending.value = undefined
    options.value = undefined
    error.value = ''
  }
  catch (cause) {
    if (operation === lifetime)
      error.value = (cause as Error).message
  }
  finally {
    if (operation === lifetime) {
      sending.value = false
      await load(generation)
    }
  }
}
</script>

<template>
  <ChatQuotaCard
    v-if="shownQuota"
    :quota="shownQuota"
    :can-manage="canManage"
    :read-only="readOnly"
    :busy="busy || sending"
    :loading="loading"
    :choices="options?.choices"
    :switching="switching ? providers[switching].label : undefined"
    :switch-error="error || loadError || options?.switching?.error || undefined"
    @resume="emit('resume')"
    @switch="choose"
    @cancel-switch="cancel"
  />
</template>
