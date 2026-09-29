<script setup lang="ts">
import type { Deliverable } from '../../shared/artifacts'
import { onBeforeUnmount, ref, watch } from 'vue'
import { artifactUrl } from '../../shared/artifacts'
import { api, notify } from '../api'

const props = defineProps<{ item: Deliverable }>()
const record = ref(props.item)
const busy = ref(true)
const error = ref('')
const controller = new AbortController()
onBeforeUnmount(() => controller.abort())
watch(() => props.item, item => record.value = item)

async function refresh() {
  try {
    record.value = await api<Deliverable>(`${artifactUrl(props.item).slice(4)}?metadata=1`, { signal: controller.signal })
  }
  catch (e) {
    if (!controller.signal.aborted)
      error.value = (e as Error).message
  }
  finally { busy.value = false }
}

async function change(visibility: 'private' | 'public') {
  busy.value = true
  error.value = ''
  try {
    record.value = await api<Deliverable>(`${artifactUrl(props.item).slice(4)}/visibility`, {
      method: 'PUT',
      body: JSON.stringify({ visibility }),
      signal: controller.signal,
    })
    notify(visibility === 'public' ? 'Public link enabled for this version.' : 'Public link disabled.')
  }
  catch (e) {
    if (!controller.signal.aborted)
      error.value = (e as Error).message
  }
  finally { busy.value = false }
}

async function copy() {
  try {
    await navigator.clipboard.writeText(record.value.publicUrl!)
    notify('Public link copied.')
  }
  catch { error.value = 'Could not copy the link. Select and copy it below.' }
}

void refresh()
</script>

<template>
  <section aria-label="File sharing" class="max-h-[55dvh] shrink-0 space-y-2 overflow-y-auto border-b border-line bg-soft px-5 py-3 phone:px-3">
    <p class="m-0! text-sm font-medium">
      {{ record.visibility === 'public' ? 'Public link enabled' : 'Private file' }}
    </p>
    <p class="m-0! text-xs text-muted">
      Anyone with a public link can read this version without signing in. Other files and versions stay private.
    </p>
    <template v-if="record.visibility === 'public' && record.publicUrl">
      <input
        :value="record.publicUrl"
        readonly
        aria-label="Public link"
        class="w-full min-w-0 rounded-lg border border-line bg-surface px-3 py-2 text-xs"
        @focus="($event.target as HTMLInputElement).select()"
      >
      <div class="flex flex-wrap gap-2">
        <button :disabled="busy" class="rounded-lg bg-brand phone:min-h-11 px-3 py-2 text-xs text-white disabled:opacity-50" @click="copy">
          Copy public link
        </button>
        <button :disabled="busy" class="rounded-lg border border-line phone:min-h-11 px-3 py-2 text-xs disabled:opacity-50" @click="change('private')">
          Disable public link
        </button>
      </div>
      <p class="m-0! text-xs text-muted">
        Disabling the link blocks future access. Downloaded copies remain with their recipients.
      </p>
    </template>
    <button
      v-else
      :disabled="busy"
      class="rounded-lg border border-line phone:min-h-11 px-3 py-2 text-xs disabled:opacity-50"
      @click="change('public')"
    >
      {{ busy ? 'Loading…' : 'Enable public link' }}
    </button>
    <p v-if="error" role="alert" class="m-0! text-xs text-danger">
      {{ error }}
    </p>
  </section>
</template>
