<script setup lang="ts">
import type { IconName } from '../icons'
import type { SelectOption } from '../select'
import { computed, onMounted, ref } from 'vue'
import { api, redirect } from '../api'
import AccountConfirmation from '../components/AccountConfirmation.vue'
import AuthStage from '../components/AuthStage.vue'
import BrandWordmark from '../components/BrandWordmark.vue'
import Icon from '../components/Icon.vue'
import UiAlert from '../components/UiAlert.vue'
import UiButton from '../components/UiButton.vue'
import VirtualSelect from '../components/VirtualSelect.vue'
import {
  Eye,
  Link,
  Lock,
  Monitor,
  Pencil,
  ShieldCheck,
  Terminal,
  TriangleAlert,
} from '../icons'

const props = defineProps<{ email?: string }>()

const parameters = Object.fromEntries(new URLSearchParams(location.search))
const details = ref<{
  client: { client_id: string, client_name: string, redirect_uri: string }
  scopes: string[]
  installations: Array<{ id: string, name: string, online: boolean }>
  confirmationRequired?: boolean
}>()
const redirectUrl = computed(() => {
  try {
    return details.value?.client.redirect_uri ? new URL(details.value.client.redirect_uri) : null
  }
  catch {
    return null
  }
})
const redirectHost = computed(() => redirectUrl.value?.host ?? '')
const isLocalRedirect = computed(() => {
  const host = redirectUrl.value?.hostname ?? ''
  return host === 'localhost' || host === '127.0.0.1' || host === '[::1]' || host === '::1'
})
const clientInitial = computed(() => {
  const match = (details.value?.client.client_name ?? '').trim().match(/[\p{L}\p{N}]/u)
  return (match?.[0] ?? 'M').toUpperCase()
})
const accountInitial = computed(() => {
  const local = (props.email ?? '').split('@')[0] ?? ''
  const match = local.match(/[\p{L}\p{N}]/u)
  return (match?.[0] ?? 'C').toUpperCase()
})
const loading = ref(true)
const error = ref('')
const busy = ref(false)
const installationId = ref('')
const installationOptions = computed<SelectOption[]>(() => (details.value?.installations ?? []).map(item => ({
  value: item.id,
  label: item.name,
  status: { label: item.online ? 'Online' : 'Offline', tone: item.online ? 'success' : 'muted' },
})))
const confirmingIdentity = ref(false)
const confirmationRequired = ref(false)
const endpoint = '/mcp/oauth'

interface ScopePresentation {
  title: string
  description: string
  icon: IconName
  elevated: boolean
}

function presentScope(scope: string): ScopePresentation {
  if (scope === 'read') {
    return {
      title: 'Read',
      description: 'Read tasks, profiles, skills, and run results',
      icon: Eye,
      elevated: false,
    }
  }

  if (scope === 'run') {
    return {
      title: 'Full execution',
      description: 'Start and cancel tasks in YOLO mode with full access on the worker',
      icon: Terminal,
      elevated: true,
    }
  }

  return {
    title: 'Manage',
    description: 'Create and modify tasks, projects, agents, and skills',
    icon: Pencil,
    elevated: false,
  }
}

onMounted(async () => {
  try {
    details.value = await api(`${endpoint}/preview`, {
      method: 'POST',
      body: JSON.stringify(parameters),
    })
    installationId.value = details.value?.installations?.[0]?.id || ''
    confirmationRequired.value = Boolean(details.value?.confirmationRequired)
  }
  catch (e) {
    error.value = (e as Error).message
  }
  finally {
    loading.value = false
  }
})

async function consent(approved: boolean) {
  busy.value = true
  error.value = ''
  try {
    const result = await api(`${endpoint}/consent`, {
      method: 'POST',
      body: JSON.stringify({ parameters, approved, installationId: installationId.value }),
    })
    redirect(result.redirect)
  }
  catch (e) {
    const message = (e as Error).message
    error.value = message
    if (/confirm your identity/i.test(message))
      confirmationRequired.value = true
    busy.value = false
  }
}

function handleConfirmed() {
  confirmingIdentity.value = false
  confirmationRequired.value = false
  error.value = ''
}
</script>

<template>
  <AuthStage max-width="md" label="Connect an assistant" :busy="busy || loading">
    <template #header>
      <BrandWordmark class="origin-left scale-[0.88]" />
      <span
        v-if="email"
        :title="`Signed in as ${email}`"
        class="inline-flex max-w-[60%] items-center gap-2 rounded-full border border-line bg-surface py-1 pr-3 pl-1.5 text-xs font-medium text-muted"
      >
        <span class="grid size-5.5 shrink-0 place-items-center rounded-full bg-soft text-[11px] font-bold text-accent" aria-hidden="true">{{ accountInitial }}</span>
        <span class="truncate">{{ email }}</span>
      </span>
    </template>

    <div class="grid gap-5">
      <div class="flex flex-col items-center text-center">
        <div class="mb-4 flex items-center justify-center gap-3.5" aria-hidden="true">
          <span
            class="grid size-13 place-items-center rounded-[15px] font-heading text-xl font-bold text-white shadow-sm"
            style="background: linear-gradient(135deg, var(--color-claude), color-mix(in srgb, var(--color-claude) 82%, #16151d));"
          >
            {{ details ? clientInitial : 'M' }}
          </span>
          <span class="flex items-center gap-1.5 text-muted">
            <span class="size-1 rounded-full bg-control" />
            <span class="size-1 rounded-full bg-control" />
            <span class="grid size-7 place-items-center rounded-full border border-line bg-inset">
              <Icon :name="Link" :size="14" />
            </span>
            <span class="size-1 rounded-full bg-control" />
            <span class="size-1 rounded-full bg-control" />
          </span>
          <span class="grid size-13 place-items-center rounded-[15px] border border-line bg-inset">
            <img class="cairn-mark-light size-7" src="/brand/cairn-symbol.svg" alt="">
            <img class="cairn-mark-dark size-7" src="/brand/cairn-symbol-dark.svg" alt="">
          </span>
        </div>

        <h1 class="font-heading text-[22px] font-bold tracking-tight">
          Connect an assistant
        </h1>
        <p v-if="details" class="mt-1.5 text-sm text-muted">
          <strong class="font-semibold text-ink">{{ details.client.client_name }}</strong>
          is requesting MCP access to one Cairn installation.
        </p>
        <p v-else-if="loading" role="status" class="mt-1.5 text-sm text-muted">
          Loading authorization request…
        </p>
      </div>

      <UiAlert v-if="error" class="my-0!">
        {{ error }}
      </UiAlert>

      <template v-if="details">
        <div
          role="alert"
          class="rounded-[13px] border border-warning/35 bg-warning-surface/70 p-3.5"
        >
          <div class="mb-2.5 flex flex-wrap items-center justify-between gap-2">
            <span class="inline-flex items-center gap-1.5 text-xs font-bold text-warning">
              <Icon :name="TriangleAlert" :size="14" />
              Unverified client
            </span>
            <span class="text-[11px] font-medium text-warning/90">
              Self-declared name · verify destination
            </span>
          </div>
          <div class="flex flex-wrap items-center justify-between gap-2 rounded-[9px] border border-line bg-surface px-3 py-2">
            <span class="text-xs text-muted">Redirect destination</span>
            <span class="inline-flex min-w-0 items-center gap-1.5">
              <Icon
                :name="isLocalRedirect ? Monitor : Lock"
                :size="13"
                :class="isLocalRedirect ? 'text-accent' : 'text-success'"
              />
              <strong class="truncate font-mono text-xs font-semibold text-ink">{{ redirectHost }}</strong>
              <span class="rounded-full border border-line bg-inset px-2 py-0.5 text-[10.5px] font-semibold text-muted">
                {{ isLocalRedirect ? 'Local app' : 'HTTPS' }}
              </span>
            </span>
          </div>
          <p class="mt-2 text-2xs leading-relaxed text-warning">
            Its name is supplied by the client and has not been checked by Cairn. Continue only if you trust this redirect destination.
          </p>
        </div>

        <div class="grid gap-1.5">
          <VirtualSelect
            v-model="installationId"
            label="Installation"
            :options="installationOptions"
            :disabled="busy"
          />
          <p v-if="!details.installations.length" class="text-xs text-muted">
            Add an installation before granting access.
          </p>
        </div>

        <div class="grid gap-2">
          <span class="text-xs font-semibold text-ink">Requested permissions</span>
          <ul class="divide-y divide-line overflow-hidden rounded-[13px] border border-line bg-inset">
            <li
              v-for="scope in details.scopes"
              :key="scope"
              class="flex items-start gap-3 px-3.5 py-3"
              :class="presentScope(scope).elevated ? 'bg-linear-to-r from-coral-soft/75 to-transparent' : ''"
            >
              <span
                class="mt-0.5 grid size-7.5 shrink-0 place-items-center rounded-[9px] border"
                :class="presentScope(scope).elevated
                  ? 'border-coral/30 bg-coral-soft text-coral'
                  : 'border-line bg-surface text-accent'"
                aria-hidden="true"
              >
                <Icon :name="presentScope(scope).icon" :size="15" />
              </span>
              <div class="min-w-0 flex-1">
                <div class="flex flex-wrap items-center gap-2 text-sm font-semibold text-ink">
                  <span>{{ presentScope(scope).title }}</span>
                  <span
                    v-if="presentScope(scope).elevated"
                    class="rounded-full border border-coral/30 bg-coral-soft px-2 py-0.5 text-[10.5px] font-bold text-coral"
                  >
                    Elevated access
                  </span>
                </div>
                <p class="mt-0.5 text-xs leading-relaxed text-muted">
                  {{ presentScope(scope).description }}
                </p>
              </div>
            </li>
          </ul>
        </div>

        <AccountConfirmation
          v-if="confirmingIdentity"
          title="Confirm identity before granting access to this assistant"
          return-label="Cancel confirmation"
          @close="confirmingIdentity = false"
          @confirmed="handleConfirmed"
        />
        <div
          v-else-if="confirmationRequired"
          class="flex flex-wrap items-center justify-between gap-3 rounded-[13px] border border-line bg-inset px-3.5 py-3"
        >
          <div class="flex min-w-0 items-start gap-2.5">
            <Icon :name="ShieldCheck" :size="16" class="mt-0.5 shrink-0 text-accent" />
            <div class="min-w-0">
              <p class="text-sm font-semibold text-ink">
                Identity confirmation required
              </p>
              <p class="text-xs text-muted">
                Verify your session with a passkey or email code before allowing access.
              </p>
            </div>
          </div>
          <UiButton
            size="small"
            class="border-transparent bg-soft text-accent enabled:hover:bg-soft/80"
            :disabled="busy"
            @click="confirmingIdentity = true"
          >
            Confirm identity
          </UiButton>
        </div>

        <div
          v-else
          class="flex items-center gap-2 rounded-[11px] border border-success/25 bg-success/10 px-3 py-2 text-xs font-semibold text-success"
        >
          <Icon :name="ShieldCheck" :size="15" class="shrink-0" />
          <span>Identity recently confirmed — ready to allow access</span>
        </div>

        <div class="flex flex-wrap items-center justify-between gap-3 border-t border-line pt-4">
          <p class="text-xs text-muted">
            You can revoke this connection at any time in Settings.
          </p>
          <div class="flex flex-wrap justify-end gap-2.5 phone:w-full">
            <UiButton class="phone:flex-1" :disabled="busy || confirmingIdentity" @click="consent(false)">
              Deny
            </UiButton>
            <UiButton
              variant="primary"
              class="phone:flex-1"
              :disabled="busy || confirmationRequired || !installationId"
              @click="consent(true)"
            >
              Allow access
            </UiButton>
          </div>
        </div>
      </template>
    </div>
  </AuthStage>
</template>
