<script setup lang="ts">
import type { Catalog, PluginEntry } from '../plugins'
import { computed, ref } from 'vue'
import { api, notify } from '../api'
import {
  Check,
  Globe,
  KeyRound,
  ShieldCheck,
  Trash2,
  TriangleAlert,
} from '../icons'
import { capabilityLabels } from '../plugins'
import Icon from './Icon.vue'
import Modal from './Modal.vue'
import PluginMark from './PluginMark.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'

const props = defineProps<{ entry: PluginEntry, capabilities: Catalog['capabilities'] }>()
const emit = defineEmits<{ close: [], changed: [] }>()

const valid = computed(() => props.entry.versions.filter(version => version.manifest))
// Review the newest version waiting for approval, otherwise the active one.
const selected = ref(
  [...valid.value].reverse().find(version => !version.approved)?.version
  ?? props.entry.active
  ?? valid.value.at(-1)?.version
  ?? '',
)
const version = computed(() => valid.value.find(item => item.version === selected.value))
const manifest = computed(() => version.value?.manifest)
const active = computed(() => valid.value.find(item => item.version === props.entry.active)?.manifest)
const added = computed(() => manifest.value?.capabilities.filter(name => active.value && !active.value.capabilities.includes(name)) ?? [])
const busy = ref(false)
const error = ref('')
const removing = ref(false)

async function act(operation: 'approve' | 'activate') {
  busy.value = true
  error.value = ''
  try {
    await api(`/plugins/packages/${props.entry.id}/${operation}`, { method: 'POST', body: JSON.stringify({ version: selected.value, digest: version.value?.digest }) })
    notify(`${manifest.value?.name} ${selected.value} is active`)
    emit('changed')
    emit('close')
  }
  catch (e) {
    error.value = (e as Error).message
  }
  finally {
    busy.value = false
  }
}

async function uninstall() {
  busy.value = true
  error.value = ''
  try {
    await api(`/plugins/packages/${props.entry.id}`, { method: 'DELETE' })
    notify(`${props.entry.manifest?.name ?? props.entry.id} uninstalled`)
    emit('changed')
    emit('close')
  }
  catch (e) {
    error.value = (e as Error).message
    removing.value = false
  }
  finally {
    busy.value = false
  }
}
</script>

<template>
  <Modal :title="`Review ${entry.manifest?.name ?? entry.id}`" wide @close="emit('close')">
    <div class="grid gap-5 px-6.5 py-6 phone:p-5">
      <UiAlert v-if="error">
        {{ error }}
      </UiAlert>
      <div v-if="manifest" class="flex items-start gap-3.5">
        <PluginMark :plugin="entry.id" :brand="manifest.brand" :size="44" />
        <div class="min-w-0">
          <p class="text-sm font-semibold">
            {{ manifest.name }} <span class="font-normal text-muted">{{ manifest.version }}</span>
          </p>
          <p class="mt-0.5 text-xs leading-relaxed text-muted">
            {{ manifest.description || 'No description.' }}
          </p>
        </div>
      </div>

      <fieldset v-if="valid.length > 1" class="grid gap-2">
        <legend class="mb-2 text-xs font-semibold">
          Versions
        </legend>
        <label
          v-for="item in valid"
          :key="item.version"
          class="flex flex-row! items-center gap-3! rounded-xl border border-line px-3.5 py-2.5 text-ink! font-normal! has-[:checked]:border-accent"
        >
          <input
            v-model="selected"
            type="radio"
            name="version"
            :value="item.version"
            class="accent-[var(--color-accent)]"
          >
          <span class="flex-1 text-xs font-semibold">{{ item.version }}</span>
          <span v-if="item.version === entry.active" class="rounded-full bg-[light-dark(#dff3ee,#163a33)] px-2 py-px text-3xs font-semibold text-success">Active</span>
          <span v-else-if="item.approved" class="text-2xs text-muted">Approved before</span>
          <span v-else class="rounded-full bg-warning-surface px-2 py-px text-3xs font-semibold text-warning">Needs approval</span>
        </label>
      </fieldset>
      <p v-for="item in entry.versions.filter(item => item.error)" :key="item.version" class="flex items-start gap-2 text-2xs text-danger">
        <Icon :name="TriangleAlert" :size="13" class="mt-px" />Version {{ item.version }} can’t be used: {{ item.error }}
      </p>

      <section v-if="manifest">
        <h3 class="text-xs! font-semibold">
          What it can do
        </h3>
        <ul class="mt-2 grid gap-2">
          <li v-for="name in manifest.capabilities" :key="name" class="flex items-start gap-3 rounded-xl border border-line bg-inset px-3.5 py-3">
            <Icon :name="capabilityLabels[name]?.[0] ?? ShieldCheck" :size="16" class="mt-px text-accent" />
            <span class="min-w-0 flex-1">
              <span class="flex items-center gap-2 text-xs font-semibold">{{ capabilityLabels[name]?.[1] ?? name }}
                <span v-if="added.includes(name)" class="rounded-full bg-warning-surface px-2 py-px text-3xs text-warning">New</span>
              </span>
              <span class="mt-0.5 block text-2xs leading-relaxed text-muted">{{ capabilities.find(item => item.name === name)?.description }}</span>
            </span>
          </li>
          <li v-if="!manifest.capabilities.length" class="text-xs text-muted">
            No access to Cairn or the network.
          </li>
        </ul>
        <p v-if="manifest.hosts?.length" class="mt-3 flex items-start gap-2 text-2xs leading-relaxed text-muted">
          <Icon :name="Globe" :size="13" class="mt-px" />Can reach only {{ manifest.hosts.join(', ') }}, over HTTPS, without following redirects.
        </p>
        <p v-if="manifest.credential" class="mt-1.5 flex items-start gap-2 text-2xs leading-relaxed text-muted">
          <Icon :name="KeyRound" :size="13" class="mt-px" />Each instance’s {{ manifest.credential.label }} is added by Cairn to its requests ({{ manifest.credential.header }} header). The plugin never sees it.
        </p>
        <p
          v-for="program in manifest.programs ?? []"
          :key="program.name"
          class="mt-1.5 flex items-start gap-2 rounded-lg bg-warning-surface px-2.5 py-2 text-2xs leading-relaxed text-warning"
        >
          <Icon :name="TriangleAlert" :size="13" class="mt-px shrink-0" /><span>Runs <code>{{ program.path }}</code> on this server, outside the Luau sandbox, with the files and network that program can reach.<template v-if="program.description"> {{ program.description }}</template></span>
        </p>
        <p v-if="manifest.agentAccess" class="mt-1.5 flex items-start gap-2 text-2xs leading-relaxed text-muted">
          <Icon :name="KeyRound" :size="13" class="mt-px" /><span>Agents you authorize on an instance get {{ manifest.commands?.length ?? 0 }} command{{ manifest.commands?.length === 1 ? '' : 's' }} and {{ manifest.tools?.length ?? 0 }} tool{{ manifest.tools?.length === 1 ? '' : 's' }}<template v-if="manifest.instructions">, and instructions in their prompt</template>. Nothing is granted by default.</span>
        </p>
        <p class="mt-1.5 flex items-start gap-2 text-2xs leading-relaxed text-muted">
          <Icon :name="ShieldCheck" :size="13" class="mt-px" />The script runs in Cairn’s Luau sandbox: no files, processes or other network access of its own. Editing any of its files on disk requires a new approval.
        </p>
        <ul v-if="version?.files?.length" class="mt-2 grid gap-0.5 font-mono text-3xs text-muted" aria-label="Package files">
          <li v-for="file in version.files" :key="file.path">
            {{ file.path }} · {{ Math.max(1, Math.ceil(file.bytes / 1024)) }} KB
          </li>
        </ul>
      </section>
    </div>
    <footer class="modal-actions sticky bottom-0 flex items-center justify-between gap-2.5 border-t border-line bg-surface px-6.5 py-4.5 phone:flex-wrap phone:px-5 phone:py-4">
      <template v-if="!entry.builtin">
        <UiButton
          v-if="!removing"
          variant="danger-outline"
          :disabled="busy"
          @click="removing = true"
        >
          <Icon :name="Trash2" :size="15" />Uninstall
        </UiButton>
        <UiButton
          v-else
          variant="danger"
          :disabled="busy"
          @click="uninstall"
        >
          Confirm uninstall
        </UiButton>
      </template>
      <span v-else />
      <div class="flex gap-2.5">
        <UiButton :disabled="busy" @click="emit('close')">
          Close
        </UiButton>
        <UiButton
          v-if="!entry.builtin && version && !version.approved"
          variant="primary"
          :disabled="busy"
          @click="act('approve')"
        >
          <Icon :name="Check" :size="15" />Approve and activate
        </UiButton>
        <UiButton
          v-else-if="!entry.builtin && version && selected !== entry.active"
          variant="primary"
          :disabled="busy"
          @click="act('activate')"
        >
          Switch to {{ selected }}
        </UiButton>
      </div>
    </footer>
  </Modal>
</template>
