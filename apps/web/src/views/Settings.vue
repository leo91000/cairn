<script setup lang="ts">
import { twMerge } from 'tailwind-merge'
import {
  computed,
  onBeforeUnmount,
  onMounted,
  ref,
} from 'vue'
import { useRoute, useRouter } from 'vue-router'
import {
  api,
  beaconEntry,
  date,
  notify,
  state,
} from '../api'
import AccountConfirmation from '../components/AccountConfirmation.vue'
import Icon from '../components/Icon.vue'
import Modal from '../components/Modal.vue'
import StorageSettings from '../components/StorageSettings.vue'
import ThemeControl from '../components/ThemeControl.vue'
import UiAlert from '../components/UiAlert.vue'
import UiButton from '../components/UiButton.vue'
import {
  Copy,
  ExternalLink,
  KeyRound,
  Plus,
} from '../icons'
import { iconButton } from '../ui'

const route = useRoute()
const router = useRouter()
const section = computed(() => String(route.params.section || ''))
const sections = [
  { id: 'account', label: 'Account' },
  { id: 'installation', label: 'Installation' },
  { id: 'sensitive', label: 'Sensitive zone' },
]
// Bare /settings shows Account beside the sections on wide screens, and only the list on phones.
const phoneQuery = window.matchMedia('(width <= 640px)')
const phone = ref(phoneQuery.matches)
const onPhone = () => phone.value = phoneQuery.matches
const current = computed(() => section.value || (phone.value ? '' : 'account'))
const settings = ref<any>()
const grants = ref<any[]>([])
const audit = ref<any[]>([])
const open = ref(false)
const confirmingIdentity = ref(false)
const label = ref('')
const scopes = ref(['read'])
const created = ref('')
const error = ref('')

async function load() {
  try {
    [settings.value, grants.value, audit.value] = await Promise.all([
      api('/settings'),
      api('/tokens'),
      api('/audit'),
    ])
    if (beaconEntry)
      settings.value.mcpUrl = `${location.origin}/mcp`
  }
  catch (e) {
    error.value = (e as Error).message
  }
}

onMounted(() => {
  phoneQuery.addEventListener('change', onPhone)
  if (section.value === 'installation' && state.installationRole === 'owner')
    void load()
})
onBeforeUnmount(() => phoneQuery.removeEventListener('change', onPhone))

// Return to the section list this screen was opened from; a deep link has no such entry.
function backToSettings() {
  if (router.options.history.state.back === '/settings')
    router.back()
  else
    void router.push('/settings')
}

async function create() {
  try {
    created.value = (
      await api('/tokens', {
        method: 'POST',
        body: JSON.stringify({ label: label.value, scopes: scopes.value }),
      })
    ).token
    await load()
  }
  catch (e) {
    error.value = (e as Error).message
  }
}

async function revoke(id: string) {
  try {
    await api(`/tokens/${id}`, { method: 'DELETE' })
    await load()
    notify('Access revoked')
  }
  catch (e) {
    error.value = (e as Error).message
  }
}

async function copy(value: string) {
  try {
    await navigator.clipboard.writeText(value)
    notify('Copied to clipboard')
  }
  catch {
    error.value = 'Clipboard unavailable. Select and copy the value manually.'
  }
}
</script>

<template>
  <div class="page-heading flex items-center justify-between gap-5 mb-[27px] phone:gap-2.5 phone:flex-wrap phone:mb-[21px]">
    <div>
      <h1>Settings</h1>
    </div>
  </div>
  <UiAlert v-if="error">
    {{ error }}
  </UiAlert>
  <div class="grid min-w-0 grid-cols-[200px_minmax(0,1fr)] gap-10 phone:grid-cols-1 phone:gap-0">
    <nav aria-label="Settings sections" class="self-start grid gap-1.5 phone:gap-3" :class="section ? 'phone:hidden' : ''">
      <RouterLink
        v-for="item in sections"
        :key="item.id"
        :to="`/settings/${item.id}`"
        class="min-h-11 rounded-lg border border-transparent px-4 py-3 text-sm font-semibold phone:border-line phone:bg-raised"
        :class="current === item.id ? 'bg-soft text-accent' : item.id === 'sensitive' ? 'text-danger hover:bg-danger-surface' : 'text-muted hover:bg-hover hover:text-ink'"
        :aria-current="current === item.id ? 'page' : undefined"
      >
        {{ item.label }}
      </RouterLink>
    </nav>
    <div class="min-w-0" :class="!section ? 'phone:hidden' : ''">
      <UiButton
        v-if="section"
        size="small"
        class="mb-5 hidden phone:inline-flex"
        @click="backToSettings"
      >
        Back to settings
      </UiButton>
      <template v-if="section === 'account' || !section">
        <h2 class="text-xl font-heading">
          Account
        </h2>
        <slot name="account" />
        <section class="panel border-b border-line overflow-hidden settings-section mb-5.5 appearance-section px-0 py-7 phone:py-5">
          <div class="section-intro flex gap-[17px] items-center mb-6 phone:items-start phone:gap-[13px]">
            <div><h2>Appearance</h2></div>
          </div>
          <ThemeControl />
          <p class="appearance-hint text-2xs text-muted mt-4">
            Saved in this browser. System follows your device’s appearance automatically.
          </p>
        </section>
      </template>
      <template v-else-if="section === 'sensitive'">
        <h2 class="mb-5 text-xl font-heading text-danger">
          Sensitive zone
        </h2>
        <slot name="sensitive" />
      </template>
      <template v-else-if="section === 'installation'">
        <slot name="installation-heading">
          <h2 class="text-xl font-heading">
            Installation
          </h2>
        </slot>
        <slot name="installation" />
        <template v-if="settings && state.installationRole === 'owner'">
          <StorageSettings />
          <section class="panel border-b border-line overflow-hidden settings-section mb-5.5 px-0 py-7 phone:py-5">
            <div class="section-intro flex gap-[17px] items-center mb-6 phone:items-start phone:gap-[13px]">
              <span class="resource-avatar w-11.5 h-11.5 bg-soft text-accent grid place-items-center rounded-xl border border-line"><Icon :name="ExternalLink" :size="22" /></span>
              <div>
                <h2>Connect from ChatGPT or Claude</h2>
                <p>
                  Add Cairn as a remote MCP server in your assistant’s connector
                  settings.
                </p>
              </div>
            </div>
            <label>MCP server URL
              <div class="copy-field flex border border-line rounded-lg items-center pr-[5px]">
                <input
                  :value="settings.mcpUrl"
                  readonly
                  aria-label="MCP server URL"
                ><button
                  :class="twMerge(iconButton, 'icon-button')"
                  aria-label="Copy MCP URL"
                  @click="copy(settings.mcpUrl)"
                >
                  <Icon :name="Copy" :size="17" />
                </button></div></label>
            <div class="inline-note bg-surface rounded-lg text-xs leading-[1.7] text-muted px-[15px] py-[13px]">
              Choose OAuth authentication. You’ll sign in here and approve exactly
              what the assistant can access. Connect using the Streamable HTTP
              transport supported by Codex and other compatible MCP clients.
            </div>
          </section>
          <section class="panel border-b border-line overflow-hidden settings-section mb-5.5 px-0 py-7 phone:py-5">
            <header class="panel-heading flex justify-between items-center gap-[15px] pt-[23px] pb-5 px-6 phone:p-[19px]">
              <div>
                <h2>Connected clients & access tokens</h2>
                <p>
                  Review and revoke access without disconnecting your agent accounts.
                </p>
              </div>
              <UiButton
                size="small"
                @click="
                  open = true;
                  created = '';
                  confirmingIdentity = false;
                  label = '';
                  scopes = ['read'];
                "
              >
                <Icon :name="Plus" :size="15" />New token
              </UiButton>
            </header>
            <div v-if="grants.length" class="grants-list">
              <div v-for="grant in grants" :key="grant.id">
                <Icon :name="KeyRound" :size="19" />
                <div class="grow flex-1 min-w-0">
                  <strong>{{ grant.label }}</strong><small>{{ grant.scopes.join(" · ") }} ·
                    {{
                      grant.clientId === "personal"
                        ? "Personal token"
                        : "OAuth client"
                    }}</small>
                </div>
                <UiButton variant="danger-outline" size="small" @click="revoke(grant.id)">
                  Revoke
                </UiButton>
              </div>
            </div>
            <p v-else class="muted text-muted">
              No external clients have access yet.
            </p>
          </section>
          <section class="panel border-b border-line overflow-hidden settings-section mb-5.5 px-0 py-7 phone:py-5">
            <h2>Worker environment</h2>
            <dl class="settings-facts grid grid-cols-[160px_1fr] gap-[17px] text-xs phone:grid-cols-[95px_minmax(0,_1fr)] phone:text-xs mx-0 my-[25px]">
              <dt>Version</dt>
              <dd>{{ settings.version }}</dd>
              <dt>Host concurrency (development)</dt>
              <dd>{{ settings.concurrency }}</dd>
              <dt>Workspace roots</dt>
              <dd>
                <code v-for="root in settings.workspaceRoots" :key="root">{{
                  root
                }}</code>
              </dd>
              <dt>Global skills</dt>
              <dd>
                <code>{{ settings.home }}/.agents/skills</code>
              </dd>
            </dl>
            <p class="muted text-muted">
              Infrastructure settings are configured through your deployment
              environment.
            </p>
          </section>
          <section class="panel border-b border-line overflow-hidden settings-section mb-5.5 px-0 py-7 phone:py-5">
            <h2>Recent changes</h2>
            <div class="audit-list mt-4.5">
              <div v-for="item in audit.slice(0, 20)" :key="item.id">
                <span>{{ item.action.replaceAll(".", " · ") }}</span><time>{{ date(item.created_at) }}</time>
              </div>
              <p v-if="!audit.length" class="muted text-muted">
                Workspace changes will be recorded here.
              </p>
            </div>
          </section>
        </template>
      </template>
    </div>
  </div>
  <Modal
    v-if="open"
    :title="created ? 'Save your access token' : 'Create an access token'"
    @close="
      open = false;
      created = '';
      confirmingIdentity = false;
    "
  >
    <div class="modal-body px-6.5 py-6 phone:p-5">
      <AccountConfirmation
        v-if="confirmingIdentity"
        title="Confirm identity before creating an access token"
        return-label="Back to access token"
        @close="confirmingIdentity = false"
        @confirmed="confirmingIdentity = false; error = ''"
      />
      <template v-if="created">
        <p>
          This token is shown once. Store it securely and use it as a Bearer
          token in your MCP client.
        </p>
        <div class="token-display bg-surface border border-line font-mono wrap-anywhere rounded-lg p-[15px] mx-0 my-4.5">
          {{ created }}
        </div>
        <UiButton @click="copy(created)">
          <Icon :name="Copy" :size="16" />Copy token
        </UiButton>
      </template>
      <form
        v-else
        v-show="!confirmingIdentity"
        class="token-form"
        @submit.prevent="create"
      >
        <label>Name<input
          v-model="label"
          required
          placeholder="e.g. My terminal assistant"
          autofocus
        ></label>
        <fieldset>
          <legend>Permissions</legend>
          <label
            v-for="scope in [
              {
                id: 'read',
                label: 'Read tasks, agents, skills, and run results',
              },
              { id: 'run', label: 'Start and cancel configured tasks' },
              {
                id: 'manage',
                label: 'Create and change tasks, profiles, and skills',
              },
            ]"
            :key="scope.id"
            class="checkbox flex-row items-center gap-2 text-xs font-normal phone:text-xs phone:leading-[1.6] mx-0 my-[9px]"
          ><input v-model="scopes" type="checkbox" :value="scope.id">{{
            scope.label
          }}</label>
        </fieldset>
        <UiAlert v-if="error">
          {{ error }}
        </UiAlert>
        <UiButton v-if="beaconEntry" @click="confirmingIdentity = true">
          Confirm identity
        </UiButton>
        <UiButton variant="primary" type="submit" :disabled="!scopes.length">
          Create token
        </UiButton>
      </form>
    </div>
  </Modal>
</template>
