<script setup lang="ts">
import {
  computed,
  inject,
  ref,
  useId,
} from 'vue'
import { state } from '../api'
import { ChevronDown, Search } from '../icons'
import { connectionTone, installationKey, routeLabel } from '../installation-context'
import { workspaceActionsKey } from '../workspace-actions'
import AccountMenu from './AccountMenu.vue'
import ConnectionSheet from './ConnectionSheet.vue'
import Icon from './Icon.vue'

// Phone header of the top-level screens: the installation replaces the brand row.
// Its name and route open the Connection sheet; search and the account stay on the right.
const installation = inject(installationKey, null)
const workspaceActions = inject(workspaceActionsKey, undefined)
const current = computed(() => installation?.current.value ?? null)
const connectionOpen = ref(false)
const trigger = ref<HTMLButtonElement>()
const id = useId()
</script>

<template>
  <div
    v-if="current"
    class="installation-header hidden items-center gap-2.5 phone:flex"
    role="group"
    aria-label="Installation"
  >
    <img
      src="/brand/cairn-app-dark.svg"
      alt=""
      width="36"
      height="36"
      class="size-9 shrink-0 rounded-[10px]"
    >
    <!-- One tap target for name and route; the live statuses are announced through aria-describedby. -->
    <button
      ref="trigger"
      type="button"
      class="flex min-h-11 min-w-0 flex-1 flex-col items-start justify-center rounded-lg text-left"
      aria-haspopup="dialog"
      :aria-expanded="connectionOpen"
      :aria-labelledby="`${id}-name`"
      :aria-describedby="`${id}-status`"
      :title="`${current.name} · ${current.status} · ${routeLabel(state.transportRoute)}`"
      @click="connectionOpen = true"
    >
      <span class="flex max-w-full min-w-0 items-center gap-1 font-heading text-[15px] font-bold leading-tight text-ink">
        <span :id="`${id}-name`" class="truncate">{{ current.name }}</span><Icon :name="ChevronDown" :size="15" class="shrink-0 text-muted" />
      </span>
      <span aria-hidden="true" class="mt-0.5 flex max-w-full min-w-0 items-center gap-1.5 text-xs text-muted">
        <span class="size-1.5 shrink-0 rounded-full" :class="connectionTone(current, state.transportRoute)" />
        <span class="truncate" :class="current.updateRequired ? 'text-warning' : ''">{{ current.status }}</span>
        <span>·</span>
        <span class="shrink-0">{{ routeLabel(state.transportRoute) }}</span>
      </span>
    </button>
    <p :id="`${id}-status`" class="sr-only">
      <span role="status" aria-label="Installation availability">{{ current.status }}</span>,
      <span
        role="status"
        aria-label="Connection route"
        :data-transport-route="state.transportRoute"
      >{{ routeLabel(state.transportRoute) }}</span>
    </p>
    <button
      type="button"
      class="grid size-11 shrink-0 place-items-center rounded-full border border-line text-muted hover:bg-hover hover:text-ink"
      aria-label="Search workspace"
      @click="workspaceActions?.search()"
    >
      <Icon :name="Search" :size="18" />
    </button>
    <AccountMenu
      :email="current.email"
      :disabled="current.disabled"
      @settings="installation?.accountSettings()"
      @sign-out="installation?.signOut()"
    />
    <ConnectionSheet v-if="connectionOpen" :return-focus="trigger" @close="connectionOpen = false" />
  </div>
</template>
