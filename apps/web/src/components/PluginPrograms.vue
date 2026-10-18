<script setup lang="ts">
import type { ProgramState } from '../plugins'
import { onMounted, ref } from 'vue'
import { api } from '../api'
import { pluginOperation } from '../plugin-operations'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'

const props = defineProps<{ plugin: string }>()
const programs = ref<ProgramState[]>([])
const busy = ref(false)
const error = ref('')

async function load() {
  programs.value = (await api<{ programs: ProgramState[] }>(`/plugins/${props.plugin}/programs`)).programs
}

onMounted(() => load().catch((e: Error) => error.value = e.message))

async function act(program: ProgramState, action: string) {
  busy.value = true
  error.value = ''
  try {
    await pluginOperation(`/plugins/${props.plugin}/programs/${program.name}/${action}`, { digest: program.digest })
    await load()
  }
  catch (e) {
    error.value = (e as Error).message
  }
  finally {
    busy.value = false
  }
}
</script>

<template>
  <section class="grid gap-3 rounded-2xl border border-line bg-surface p-4" aria-label="Installed programs">
    <h2 class="text-sm! font-semibold">
      Installed programs
    </h2>
    <p v-if="busy" role="status" class="text-xs text-muted">
      Preparing program…
    </p>
    <UiAlert v-if="error">
      {{ error }}
    </UiAlert>
    <div v-for="program in programs" :key="program.name" class="grid min-w-0 gap-2 text-xs">
      <p><b>{{ program.name }}</b> · {{ program.installed ? `Version ${program.installed.release.version}` : 'Not installed · prepared on first use' }}</p>
      <p v-if="program.candidate" class="break-all text-muted">
        Available: {{ program.candidate.version }} · {{ program.candidate.url }}
      </p>
      <p v-if="program.candidate" class="text-muted">
        {{ program.candidate.sha256 ? 'The publisher’s checksum will be verified.' : 'This release uses HTTPS provenance; its checksum will be recorded when downloaded.' }}
      </p>
      <div class="flex flex-wrap gap-2">
        <UiButton
          size="small"
          variant="default"
          :disabled="busy"
          @click="act(program, 'check')"
        >
          Check {{ program.name }} for updates
        </UiButton>
        <UiButton
          v-if="program.candidate"
          size="small"
          :disabled="busy"
          @click="act(program, 'install')"
        >
          Install {{ program.name }} {{ program.candidate.version }}
        </UiButton>
        <UiButton
          v-if="program.previous"
          size="small"
          variant="default"
          :disabled="busy"
          @click="act(program, 'rollback')"
        >
          Restore {{ program.name }} {{ program.previous.release.version }}
        </UiButton>
      </div>
    </div>
  </section>
</template>
