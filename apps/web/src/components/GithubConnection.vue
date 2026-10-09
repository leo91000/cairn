<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue'
import { api, notify } from '../api'
import { ArrowUpRight, CheckCircle2, Github } from '../icons'
import { buttonBase, buttonSizes, buttonVariants } from '../ui'
import Icon from './Icon.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'

// GitHub through the server's `gh` login, with its device-code sign-in.
interface Status {
  provider: 'github'
  installed: boolean
  connected: boolean
  account: string
  version: string
  workflowPermission: boolean | null
}

interface Flow {
  state: 'pending' | 'complete' | 'failed'
  url?: string
  code?: string
  error?: string
}

const status = ref<Status>()
const flow = ref<Flow | null>(null)
const busy = ref(false)
const error = ref('')

async function load(refresh = false) {
  try {
    status.value = (await api<Status[]>(`/connections${refresh ? '?refresh=true' : ''}`)).find(item => item.provider === 'github')
  }
  catch (e) {
    error.value = (e as Error).message
  }
}

async function connect() {
  busy.value = true
  error.value = ''
  try {
    flow.value = await api<Flow>('/connections/login', { method: 'POST', body: JSON.stringify({ provider: 'github' }) })
  }
  catch (e) {
    error.value = (e as Error).message
  }
  finally {
    busy.value = false
  }
}

async function cancel() {
  await api('/connections/login', { method: 'DELETE' })
  flow.value = null
}

let timer: ReturnType<typeof setInterval>
onMounted(() => {
  void load(true)
  timer = setInterval(async () => {
    if (flow.value?.state !== 'pending')
      return
    try {
      flow.value = await api<Flow | null>('/connections/login')
      if (flow.value?.state === 'complete') {
        notify('GitHub connected')
        await load(true)
      }
    }
    catch (e) {
      error.value = (e as Error).message
    }
  }, 2000)
})
onBeforeUnmount(() => clearInterval(timer))
</script>

<template>
  <section class="grid gap-3 border-b border-line px-4.5 py-4 last:border-0 phone:px-4" aria-label="GitHub">
    <div class="flex items-center gap-3">
      <span class="grid size-[38px] shrink-0 place-items-center rounded-[30%] bg-hover text-ink"><Icon :name="Github" :size="19" /></span>
      <div class="min-w-0 flex-1">
        <h3 class="text-sm font-semibold">
          GitHub
        </h3>
        <p class="truncate text-xs text-muted">
          {{ status?.connected ? status.account : status && !status.installed ? 'The gh CLI is not installed on the server' : 'Repositories, pull requests and releases' }}
        </p>
      </div>
      <span v-if="status?.connected && status.workflowPermission !== false" class="inline-flex items-center gap-1.5 text-xs font-semibold text-success"><Icon :name="CheckCircle2" :size="14" />Connected</span>
      <UiButton
        v-if="!status?.connected || status.workflowPermission === false"
        size="small"
        :disabled="busy || !status?.installed || flow?.state === 'pending'"
        @click="connect"
      >
        {{ status?.connected ? 'Enable workflow updates' : 'Connect' }}<Icon :name="ArrowUpRight" :size="14" />
      </UiButton>
    </div>
    <p v-if="status?.connected && status.workflowPermission === false" class="text-xs text-warning">
      Reconnect to allow agents to update GitHub Actions workflows.
    </p>
    <UiAlert v-if="error || flow?.error">
      {{ error || flow?.error }}
    </UiAlert>
    <div v-if="flow?.state === 'pending'" class="grid gap-3 rounded-2xl bg-inset p-4">
      <p class="text-sm">
        Open GitHub and enter this code. Your password stays with GitHub.
      </p>
      <code v-if="flow.code" class="font-mono text-2xl font-bold tracking-widest">{{ flow.code }}</code>
      <p v-else class="text-xs text-muted">
        Waiting for a verification code…
      </p>
      <div class="flex flex-wrap gap-2">
        <a
          v-if="flow.url"
          :class="[buttonBase, buttonVariants.primary, buttonSizes.small]"
          :href="flow.url"
          target="_blank"
          rel="noopener noreferrer"
        >Open GitHub<Icon :name="ArrowUpRight" :size="14" /></a>
        <UiButton size="small" @click="cancel">
          Cancel sign-in
        </UiButton>
      </div>
    </div>
  </section>
</template>
