<script setup lang="ts">
import type { AccessRow } from '../agent-use'
import { computed, inject } from 'vue'
import { state } from '../api'
import {
  Info,
  LoaderCircle,
  TriangleAlert,
  Users,
} from '../icons'
import { installationKey } from '../installation-context'
import AccessChip from './AccessChip.vue'
import Icon from './Icon.vue'
import PersonAvatar from './PersonAvatar.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'
import UiSwitch from './UiSwitch.vue'

// Who may use this agent: the owner always, members only when the owner allows it. Each
// switch applies at once; nothing here belongs to the page's unsaved changes.
const props = defineProps<{
  agentName: string
  isNew: boolean
  // The agent manages the whole installation: using it reaches every other agent.
  installation: boolean
  rows: AccessRow[]
  // Pending invitations: access is chosen once they join.
  invited: Array<{ id: string, email: string }>
  loading: boolean
  loaded: boolean
  error: string
  // The installation predates account access: every member can use every agent.
  outdated: boolean
  saving: string[]
  failure: { accountId: string, message: string } | null
}>()
const emit = defineEmits<{ retry: [], change: [accountId: string, allow: boolean] }>()
const controls = inject(installationKey, null)
const ownerEmail = computed(() => controls?.current.value?.email || 'Installation owner')
const members = computed(() => props.rows.filter(row => row.known))
const leftovers = computed(() => props.rows.filter(row => !row.known))
const name = computed(() => props.agentName || 'this agent')
// The cause, when it says more than the sentence above.
const reason = computed(() => props.error === 'Members could not be loaded.' ? '' : props.error)
</script>

<template>
  <div class="grid min-w-0 gap-5">
    <p class="max-w-160 text-sm leading-relaxed text-muted">
      People you allow here can start conversations with {{ name }}, continue and steer them, answer its questions and schedule its tasks. Everyone in the installation still sees every conversation and can pause or stop work.
    </p>

    <div class="flex items-start gap-3 rounded-xl bg-soft px-4.5 py-3.5 text-xs leading-relaxed">
      <Icon :name="Info" :size="16" class="mt-px shrink-0 text-accent" />
      <p>
        <span class="font-semibold text-ink">The agent acts as a role.</span>
        Whoever can use it gets everything it can do: its projects, secrets, tools and powers. Give access to people you would trust with all of that.
      </p>
    </div>

    <div v-if="installation" class="flex items-start gap-3 rounded-xl border border-power/30 bg-power-surface/40 px-4.5 py-3.5 text-xs leading-relaxed" role="note">
      <Icon :name="TriangleAlert" :size="16" class="mt-px shrink-0 text-power" />
      <p>
        <span class="font-semibold text-ink">This agent manages the whole installation.</span>
        Through it, anyone who can use it can work with and change other agents, as if they could use every agent.
      </p>
    </div>

    <p v-if="isNew" class="rounded-xl border border-dashed border-line px-5 py-4 text-sm text-muted">
      Only you can use a new agent. Create it first, then choose who else can use it here.
    </p>

    <p v-else-if="outdated" class="flex items-start gap-3 rounded-xl border border-line bg-surface px-5 py-4 text-sm" role="note">
      <Icon :name="Users" :size="18" class="mt-0.5 shrink-0 text-muted" />
      <span>
        Every member can use {{ name }} on this version of the installation.
        <span class="text-muted">Update the installation to choose who can use it.</span>
      </span>
    </p>

    <template v-else>
      <div v-if="error" class="grid justify-items-start gap-3 rounded-xl border border-line bg-surface p-5" role="alert">
        <p class="text-sm">
          Members could not be loaded, so access to this agent cannot be shown or changed.
        </p>
        <p v-if="reason" class="text-sm text-muted">
          {{ reason }}
        </p>
        <UiButton size="small" :disabled="loading" @click="emit('retry')">
          {{ loading ? 'Loading…' : 'Try again' }}
        </UiButton>
      </div>

      <ul
        v-else
        class="min-w-0 divide-y divide-line overflow-hidden rounded-xl border border-line bg-surface"
        aria-label="People who can use this agent"
        :aria-busy="loading && !loaded"
      >
        <li class="flex min-h-16 items-center gap-3.5 px-4.5 py-3 phone:px-4">
          <PersonAvatar :name="ownerEmail" :identity="state.accountId || ownerEmail" :size="34" />
          <span class="min-w-0 flex-1">
            <span class="block truncate text-sm font-semibold">You</span>
            <span class="block truncate text-xs text-muted">{{ ownerEmail }} · Owner</span>
          </span>
          <AccessChip tone="fixed">
            Always
          </AccessChip>
        </li>

        <template v-if="loading && !loaded">
          <li
            v-for="index in 2"
            :key="`loading-${index}`"
            class="flex min-h-16 items-center gap-3.5 px-4.5 py-3 phone:px-4"
            aria-hidden="true"
          >
            <span class="size-8.5 shrink-0 animate-pulse rounded-full bg-hover" />
            <span class="grid flex-1 gap-1.5">
              <span class="h-3 w-44 max-w-full animate-pulse rounded bg-hover" />
              <span class="h-2.5 w-28 animate-pulse rounded bg-hover" />
            </span>
          </li>
        </template>

        <li v-for="row in [...members, ...leftovers]" :key="row.accountId" class="grid min-h-16 min-w-0 grid-cols-[minmax(0,1fr)] gap-1 px-4.5 py-3 phone:px-4">
          <div class="flex min-w-0 items-center gap-3.5">
            <PersonAvatar :name="row.known ? row.email : '?'" :identity="row.accountId" :size="34" />
            <span class="min-w-0 flex-1">
              <span class="block truncate text-sm" :class="row.known ? 'font-medium' : 'italic text-muted'">{{ row.email }}</span>
              <span class="block truncate text-xs" :class="row.known && row.allowed ? 'text-success' : 'text-muted'">
                {{ !row.known ? 'Access ends at the next membership check' : row.allowed ? 'Can use this agent' : 'No access' }}
              </span>
            </span>
            <Icon
              v-if="saving.includes(row.accountId)"
              :name="LoaderCircle"
              :size="16"
              class="shrink-0 animate-spin text-muted"
            />
            <UiSwitch
              :model-value="row.allowed"
              :label="row.known ? `Allow ${row.email} to use ${name}` : `Remove the access a former member kept to ${name}`"
              :disabled="saving.includes(row.accountId) || (!row.known && !row.allowed)"
              @update:model-value="emit('change', row.accountId, $event)"
            />
          </div>
          <p v-if="failure?.accountId === row.accountId" class="pl-12 text-xs text-danger" role="alert">
            {{ failure.message }}
          </p>
        </li>

        <li v-for="invitation in invited" :key="`invited-${invitation.id}`" class="flex min-h-16 items-center gap-3.5 px-4.5 py-3 phone:px-4">
          <PersonAvatar :name="invitation.email" :identity="invitation.email" :size="34" />
          <span class="min-w-0 flex-1">
            <span class="block truncate text-sm font-medium">{{ invitation.email }}</span>
            <span class="block truncate text-xs text-muted">Choose their access once they join</span>
          </span>
          <AccessChip tone="none">
            Invited
          </AccessChip>
        </li>

        <li v-if="loaded && !members.length && !leftovers.length && !invited.length" class="flex items-start gap-3.5 px-4.5 py-4 text-sm text-muted phone:px-4">
          <Icon :name="Users" :size="18" class="mt-0.5 shrink-0" />
          <span>No members yet. Invite people from <span class="font-medium text-ink">Members &amp; invitations</span> in the installation's settings, then choose here who can use {{ name }}.</span>
        </li>
      </ul>

      <UiAlert v-if="failure && !rows.some(row => row.accountId === failure?.accountId)" class="my-0">
        {{ failure.message }}
      </UiAlert>

      <p v-if="!error" class="text-xs leading-relaxed text-muted">
        Changes apply right away; messages a member already queued are still sent unless removed from the queue. A member without access keeps reading conversations; their scheduled tasks on this agent are suspended until access returns.
      </p>
    </template>
  </div>
</template>
