<script setup lang="ts">
import { computed, inject, ref } from 'vue'
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
    <div class="min-w-0 flex-1 cursor-pointer" @click="connectionOpen = true">
      <button
        ref="trigger"
        type="button"
        class="flex max-w-full min-w-0 items-center gap-1 text-left font-heading text-[15px] font-bold leading-tight text-ink"
        aria-haspopup="dialog"
        :aria-expanded="connectionOpen"
        :title="current.name"
        @click.stop="connectionOpen = true"
      >
        <span class="truncate">{{ current.name }}</span><Icon :name="ChevronDown" :size="15" class="shrink-0 text-muted" />
      </button>
      <p class="m-0! mt-0.5! flex min-w-0 items-center gap-1.5 text-xs text-muted" :title="`${current.status} · ${routeLabel(state.transportRoute)}`">
        <span aria-hidden="true" class="size-1.5 shrink-0 rounded-full" :class="connectionTone(current, state.transportRoute)" />
        <span
          role="status"
          aria-label="Installation availability"
          class="truncate"
          :class="current.updateRequired ? 'text-warning' : ''"
        >{{ current.status }}</span>
        <span aria-hidden="true">·</span>
        <span
          role="status"
          aria-label="Connection route"
          :data-transport-route="state.transportRoute"
          class="shrink-0"
        >{{ routeLabel(state.transportRoute) }}</span>
      </p>
    </div>
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
