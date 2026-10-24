<script setup lang="ts">
import type { PluginPage, PluginPersonalAuthorization } from '../plugins'
import {
  computed,
  onBeforeUnmount,
  ref,
  watch,
} from 'vue'
import { api, state } from '../api'
import PluginAuthorization from '../components/PluginAuthorization.vue'
import UiAlert from '../components/UiAlert.vue'
import UiButton from '../components/UiButton.vue'
import { personalAuthorizationPath } from '../plugins'

const items = ref<PluginPersonalAuthorization[]>([])
const cursor = ref<string | null>()
const busy = ref(false)
const loaded = ref(false)
const error = ref('')
const identity = computed(() => `${state.installationId}:${state.accountId}`)
let generation = 0

async function load(more = false) {
  const current = ++generation
  busy.value = true
  error.value = ''
  try {
    // Reconcile every previously visible ID so another row's completion does
    // not unmount later-page widgets and discard their memory-only consent.
    const remaining = new Set(items.value.map(item => item.id))
    const refreshed = more ? [...items.value] : []
    let nextCursor = more ? cursor.value : undefined
    for (;;) {
      const page = await api<PluginPage<PluginPersonalAuthorization>>(`/plugins/personal-authorizations${nextCursor ? `?cursor=${encodeURIComponent(nextCursor)}` : ''}`)
      if (current !== generation)
        return
      refreshed.push(...page.items)
      for (const item of page.items)
        remaining.delete(item.id)
      nextCursor = page.nextCursor
      if (more || !remaining.size || !nextCursor)
        break
    }

    items.value = refreshed
    cursor.value = nextCursor
    loaded.value = true
  }
  catch (e) {
    if (current !== generation)
      return
    error.value = (e as Error).message
  }
  finally {
    if (current === generation)
      busy.value = false
  }
}

watch(identity, () => {
  items.value = []
  cursor.value = undefined
  loaded.value = false
  void load()
}, { immediate: true })
onBeforeUnmount(() => generation++)
</script>

<template>
  <div class="page-heading mb-6 flex flex-wrap items-end justify-between gap-4">
    <div>
      <h1>Your plugin accounts</h1><p class="mt-2 text-sm text-muted">
        Personal sign-ins for this installation. Each request uses its own author’s authorization.
      </p>
    </div>
    <UiButton :disabled="busy" @click="load()">
      Refresh
    </UiButton>
  </div>
  <p class="mb-5 text-sm text-muted">
    Signing in does not grant agents access. The owner must separately allow the tools and resources they may use.
  </p>
  <UiAlert v-if="error" class="mb-4">
    {{ error }}
  </UiAlert>
  <p v-if="!loaded && !error" role="status" class="text-sm text-muted">
    Loading personal connections…
  </p>
  <p v-if="loaded && !items.length" class="text-sm text-muted">
    No personal connections are available. The owner needs to configure a plugin resource first.
  </p>
  <div class="grid min-w-0 gap-4">
    <article
      v-for="item in items"
      :key="`${identity}:${item.id}`"
      :aria-label="`${item.resourceName} · ${item.title}`"
      class="grid min-w-0 gap-3 rounded-2xl border border-line bg-surface p-5"
    >
      <header>
        <h2 class="break-words text-base! font-bold">
          {{ item.resourceName }}
        </h2><p class="break-words text-xs text-muted">
          {{ item.pluginName }} · {{ item.instanceName }} · {{ item.title }}
        </p>
      </header>
      <PluginAuthorization
        :path="personalAuthorizationPath(item)"
        :title="item.title"
        :scopes="item.scopes"
        :initial="item.authorization"
        :enabled="item.enabled"
        @changed="load()"
      />
    </article>
    <UiButton v-if="cursor" :disabled="busy" @click="load(true)">
      More personal connections
    </UiButton>
  </div>
</template>
