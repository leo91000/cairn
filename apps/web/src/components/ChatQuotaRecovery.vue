<script setup lang="ts">
import type { Provider } from '../../../../packages/contracts/accounts'
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
  busy?: boolean
}>()
const emit = defineEmits<{ resume: [], switched: [provider: Provider] }>()

interface Options {
  runId: string | null
  turnId: string | null
  provider: Provider
  choices: { provider: Provider, label: string }[]
  switching: { provider: Provider, error: string | null } | null
}

const options = ref<Options>()
const pending = ref<Provider>()
const error = ref('')
const sending = ref(false)
let generation = 0
let timer: ReturnType<typeof setTimeout> | undefined
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
  try {
    const result = await api<Options>(`/chats/${props.chatId}/coding-agent`)
    if (epoch !== generation)
      return
    options.value = result
    if (result.switching && !pending.value)
      pending.value = result.switching.provider
    if (pending.value && !result.switching && !sending.value) {
      if (result.provider === pending.value)
        emit('switched', pending.value)
      pending.value = undefined
    }

    if (result.switching || pending.value)
      timer = setTimeout(() => void load(epoch), 1000)
  }
  catch (cause) {
    if (epoch !== generation)
      return
    error.value = (cause as Error).message
    if (switching.value)
      timer = setTimeout(() => void load(epoch), 2000)
  }
}

watch(() => [props.chatId, !!props.quota, props.progress], (_, previous) => {
  const epoch = ++generation
  clearTimeout(timer)
  sending.value = false
  if (!previous || previous[0] !== props.chatId) {
    options.value = undefined
    pending.value = undefined
    error.value = ''
  }

  if (props.quota || props.progress || pending.value)
    void load(epoch)
}, { immediate: true })
onBeforeUnmount(() => {
  generation++
  clearTimeout(timer)
})

async function choose(provider: Provider) {
  const selected = options.value
  if (!selected?.runId || !selected.turnId || sending.value || switching.value)
    return
  const epoch = generation
  sending.value = true
  error.value = ''
  pending.value = provider
  try {
    await api(`/chats/${props.chatId}/coding-agent`, {
      method: 'POST',
      body: JSON.stringify({
        requestId: crypto.randomUUID(),
        runId: selected.runId,
        turnId: selected.turnId,
        provider,
      }),
    })
    if (epoch !== generation)
      return
    sending.value = false
    await load(epoch)
  }
  catch (cause) {
    if (epoch !== generation)
      return
    pending.value = undefined
    error.value = (cause as Error).message
    await load(epoch)
  }
  finally {
    if (epoch === generation)
      sending.value = false
  }
}

async function cancel() {
  try {
    await api(`/chats/${props.chatId}/stop`, { method: 'POST' })
    pending.value = undefined
    options.value = undefined
    clearTimeout(timer)
    await load(generation)
  }
  catch (cause) { error.value = (cause as Error).message }
}
</script>

<template>
  <ChatQuotaCard
    v-if="shownQuota"
    :quota="shownQuota"
    :can-manage="canManage"
    :busy="busy || sending"
    :choices="options?.choices"
    :switching="switching ? providers[switching].label : undefined"
    :switch-error="error || options?.switching?.error || undefined"
    @resume="emit('resume')"
    @switch="choose"
    @cancel-switch="cancel"
  />
</template>
