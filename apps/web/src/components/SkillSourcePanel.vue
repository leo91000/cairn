<script setup lang="ts">
import type { SkillBundle, SkillSource, SkillSourceStatus } from '../../../../packages/contracts/contracts'
import { computed, ref } from 'vue'
import { api, ApiError, date } from '../api'
import {
  ArrowUpRight,
  CircleCheck,
  GitBranch,
  GitCommit,
  Github,
  LoaderCircle,
  RefreshCw,
  TriangleAlert,
  Unlink,
} from '../icons'
import { plural, shortCommit } from '../skill-bundle'
import Icon from './Icon.vue'
import SkillDiff from './SkillDiff.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'

// Where a GitHub-imported skill came from, whether that ref moved, and what an update changes.
const props = defineProps<{
  scope: string
  name: string
  revision: string
  source: SkillSource
  unsaved: number
  readonly: boolean
}>()
const emit = defineEmits<{ replaced: [bundle: SkillBundle], detached: [], conflict: [] }>()
const status = ref<SkillSourceStatus>()
const busy = ref<'' | 'check' | 'update' | 'detach'>('')
const error = ref('')
const confirmUpdate = ref(false)
const confirmDetach = ref(false)
const base = computed(() => `/skills/${encodeURIComponent(props.scope)}/${encodeURIComponent(props.name)}/source`)
const link = computed(() => `https://github.com/${props.source.repository}/tree/${props.source.commit}${props.source.path ? `/${props.source.path}` : ''}`)
const risky = computed(() => props.source.modified || props.unsaved > 0)

async function run(kind: 'check' | 'update' | 'detach', action: () => Promise<void>) {
  busy.value = kind
  error.value = ''
  try {
    await action()
  }
  catch (e) {
    if (e instanceof ApiError && e.status === 409)
      emit('conflict')
    error.value = (e as Error).message
  }
  finally {
    busy.value = ''
  }
}

function check() {
  return run('check', async () => {
    status.value = await api<SkillSourceStatus>(base.value)
  })
}

function update() {
  if (risky.value && !confirmUpdate.value) {
    confirmUpdate.value = true
    return
  }

  return run('update', async () => {
    const bundle = await api<SkillBundle>(`${base.value}/update`, {
      method: 'POST',
      body: JSON.stringify({ commit: status.value!.latest.commit, base: props.revision }),
    })
    status.value = undefined
    confirmUpdate.value = false
    emit('replaced', bundle)
  })
}

function detach() {
  if (!confirmDetach.value) {
    confirmDetach.value = true
    return
  }

  return run('detach', async () => {
    await api(base.value, { method: 'DELETE' })
    confirmDetach.value = false
    emit('detached')
  })
}
</script>

<template>
  <div class="flex min-h-0 flex-1 flex-col overflow-auto overscroll-contain">
    <div class="mx-auto grid w-full max-w-[860px] gap-5 px-6 py-6 phone:px-4 phone:py-5">
      <section class="rounded-2xl border border-line bg-surface p-5 phone:p-4">
        <div class="flex items-start gap-3.5">
          <span class="grid size-11 shrink-0 place-items-center rounded-xl bg-ink text-canvas"><Icon :name="Github" :size="22" /></span>
          <div class="min-w-0 flex-1">
            <p class="m-0! text-3xs font-semibold tracking-[0.08em] text-subtle uppercase">
              GitHub source
            </p>
            <a
              :href="link"
              target="_blank"
              rel="noreferrer"
              class="mt-0.5 inline-flex max-w-full items-center gap-1 font-heading text-lg font-[650] text-ink hover:underline"
            >
              <span class="truncate">{{ source.repository }}</span><Icon :name="ArrowUpRight" :size="15" class="shrink-0 text-muted" />
            </a>
            <dl class="m-0! mt-3 grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-1.5 text-xs">
              <dt class="text-muted">
                Folder
              </dt><dd class="m-0 truncate font-mono text-ink">
                {{ source.path || '/' }}
              </dd>
              <dt class="text-muted">
                Ref
              </dt><dd class="m-0 flex items-center gap-1.5 font-mono text-ink">
                <Icon :name="GitBranch" :size="13" class="text-muted" />{{ source.ref }}
              </dd>
              <dt class="text-muted">
                Commit
              </dt><dd class="m-0 flex items-center gap-1.5 font-mono text-ink">
                <Icon :name="GitCommit" :size="13" class="text-muted" />{{ shortCommit(source.commit) }}
              </dd>
              <dt class="text-muted">
                Imported
              </dt><dd class="m-0 text-ink">
                {{ date(source.importedAt) }}
              </dd>
            </dl>
          </div>
        </div>
        <div v-if="source.modified" class="mt-4 flex items-start gap-2.5 rounded-xl bg-warning-surface px-3.5 py-3 text-xs leading-relaxed text-warning">
          <Icon :name="TriangleAlert" :size="16" class="mt-px shrink-0" />
          <span>This skill was edited in Cairn since its last import. Updating from GitHub replaces those edits.</span>
        </div>
        <div v-if="!readonly" class="mt-4 flex flex-wrap items-center gap-2.5">
          <UiButton size="small" :disabled="!!busy" @click="check">
            <Icon
              v-if="busy === 'check'"
              :name="LoaderCircle"
              :size="14"
              class="animate-spin"
            />
            <Icon v-else :name="RefreshCw" :size="14" />Check for updates
          </UiButton>
          <UiButton
            size="small"
            variant="danger-outline"
            :disabled="!!busy"
            @click="detach"
          >
            <Icon :name="Unlink" :size="14" />{{ confirmDetach ? 'Confirm detach' : 'Detach source' }}
          </UiButton>
          <span v-if="confirmDetach" class="text-2xs text-muted">Files stay; Cairn stops tracking {{ source.repository }}.</span>
        </div>
      </section>

      <UiAlert v-if="error">
        {{ error }}
      </UiAlert>

      <section v-if="status && status.current" class="flex items-center gap-3 rounded-2xl border border-line bg-surface px-5 py-4" role="status">
        <Icon :name="CircleCheck" :size="20" class="text-success" />
        <div>
          <p class="m-0! text-sm font-semibold">
            Up to date
          </p>
          <p class="m-0! text-xs text-muted">
            {{ source.ref }} still points at {{ shortCommit(status.latest.commit) }}.
          </p>
        </div>
      </section>

      <section v-else-if="status" class="grid gap-3.5" aria-label="Available update">
        <div class="flex items-start gap-3.5 rounded-2xl border border-accent/30 bg-soft px-5 py-4 phone:flex-col">
          <div class="min-w-0 flex-1">
            <p class="m-0! text-3xs font-semibold tracking-[0.08em] text-accent uppercase">
              Update available
            </p>
            <p class="m-0! mt-1 text-sm font-semibold text-ink">
              {{ status.latest.message.split('\n')[0] || 'Latest commit' }}
            </p>
            <p class="m-0! mt-0.5 flex flex-wrap items-center gap-x-2 text-xs text-muted">
              <span class="inline-flex items-center gap-1 font-mono"><Icon :name="GitCommit" :size="13" />{{ shortCommit(source.commit) }} → {{ shortCommit(status.latest.commit) }}</span>
              <span>· {{ date(Date.parse(status.latest.date)) }}</span>
              <span>· {{ plural(status.changes.length, 'file') }} changed</span>
            </p>
          </div>
          <UiButton
            v-if="!readonly"
            variant="primary"
            size="small"
            :disabled="!!busy"
            @click="update"
          >
            <Icon
              v-if="busy === 'update'"
              :name="LoaderCircle"
              :size="14"
              class="animate-spin"
            />
            {{ confirmUpdate ? 'Replace my edits' : `Update to ${shortCommit(status.latest.commit)}` }}
          </UiButton>
        </div>
        <div v-if="confirmUpdate" class="flex items-start gap-2.5 rounded-xl bg-warning-surface px-3.5 py-3 text-xs leading-relaxed text-warning" role="alert">
          <Icon :name="TriangleAlert" :size="16" class="mt-px shrink-0" />
          <span>
            {{ unsaved ? `You have ${plural(unsaved, 'unsaved change')} in this editor. ` : '' }}{{ source.modified ? 'Edits made in Cairn since the import will be lost. ' : '' }}Select “Replace my edits” to continue.
          </span>
        </div>
        <SkillDiff
          v-for="(change, index) in status.changes"
          :key="change.path"
          :change="change"
          :open="index < 3"
        />
      </section>
    </div>
  </div>
</template>
