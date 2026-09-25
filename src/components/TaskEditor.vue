<script setup lang="ts">
import type { Task } from '../../shared/contracts'
import { computed, ref, watch } from 'vue'
import { MAIN_AGENT_ID } from '../../shared/constants'
import {
  api,
  notify,
  refresh,
  state,
} from '../api'
import {
  Bot,
  CalendarDays,
  CalendarRange,
  Clock,
  FolderGit2,
  Play,
} from '../icons'
import Icon from './Icon.vue'
import Modal from './Modal.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'
import VirtualSelect from './VirtualSelect.vue'

const props = defineProps<{ task?: Task }>()
const emit = defineEmits<{ close: [], saved: [task: Task] }>()
const form = ref({
  name: props.task?.name ?? '',
  prompt: props.task?.prompt ?? '',
  agentId: props.task?.agentId ?? MAIN_AGENT_ID,
  projectId: props.task?.projectId ?? null,
  skills: props.task?.skills ?? null,
  tags: props.task?.tags ?? [],
  cron: props.task?.cron ?? (null as string | null),
  timezone:
    props.task?.timezone ?? Intl.DateTimeFormat().resolvedOptions().timeZone,
  enabled: props.task?.enabled ?? true,
  archived: props.task?.archived ?? false,
  worktree: props.task?.worktree ?? true,
})
const cadence = ref(props.task?.cron ? 'custom' : 'once')
const agents = computed(() => state.agents.map(agent => ({ value: agent.id, label: agent.name, description: agent.description || `${agent.model || 'Codex default'} · ${agent.reasoning || 'default'} reasoning` })))
const selectedAgent = computed(() => state.agents.find(agent => agent.id === form.value.agentId))
const allowedProjects = computed(() => state.projects.filter(project => selectedAgent.value?.access?.projects === null || selectedAgent.value?.access.projects?.includes(project.id)))
const projects = computed(() => [{ value: '', label: 'Let the agent choose', description: 'Work across its allowed projects' }, ...allowedProjects.value.map(project => ({ value: project.id, label: project.name, description: project.path }))])
const projectChoice = computed({
  get: () => form.value.projectId ?? '',
  set: (value: string) => {
    form.value.projectId = value || null
  },
})
const advanced = ref(!!props.task?.projectId || !!props.task?.skills?.length)
const inheritedSkills = computed({
  get: () => form.value.skills === null,
  set: (value: boolean) => {
    form.value.skills = value ? null : []
  },
})
const scopeDescription = computed(() => selectedAgent.value?.access.projects === null ? 'All projects' : `${allowedProjects.value.length} allowed ${allowedProjects.value.length === 1 ? 'project' : 'projects'}`)
const cadences = [
  {
    value: 'once',
    label: 'One-off',
    description: 'Run when you’re ready',
    icon: Play,
    group: 'On demand',
  },
  {
    value: 'daily',
    label: 'Every day',
    description: 'Daily at 09:00 in your timezone',
    icon: CalendarDays,
    group: 'Recurring',
  },
  {
    value: 'weekly',
    label: 'Every Monday',
    description: 'Weekly at 09:00 in your timezone',
    icon: CalendarRange,
    group: 'Recurring',
  },
  {
    value: 'custom',
    label: 'Custom schedule',
    description: 'Set your own cron expression',
    icon: Clock,
    group: 'Recurring',
  },
]
const tagText = computed({
  get: () => form.value.tags.join(', '),
  set: (value) => {
    form.value.tags = value
      .split(',')
      .map(tag => tag.trim())
      .filter(Boolean)
  },
})
const busy = ref(false)
const error = ref('')
const occurrences = ref<number[]>([])
const available = computed(() =>
  state.skills.filter(
    s =>
      s.valid && (s.scope === 'global' || (form.value.projectId ? s.scope === form.value.projectId : allowedProjects.value.some(project => project.id === s.scope))) && (selectedAgent.value?.access?.skills === null || selectedAgent.value?.access.skills?.includes(`${s.scope}/${s.name}`)),
  ),
)
watch(
  () => [form.value.cron, form.value.timezone],
  () => {
    occurrences.value = []
  },
)
watch(
  () => [form.value.agentId, form.value.projectId],
  () => {
    const keys = new Set(
      available.value.map(skill => `${skill.scope}/${skill.name}`),
    )
    if (form.value.skills)
      form.value.skills = form.value.skills.filter(key => keys.has(key))
    if (!allowedProjects.value.some(project => project.id === form.value.projectId))
      form.value.projectId = null
  },
)
watch(cadence, (value) => {
  form.value.cron
    = value === 'once'
      ? null
      : value === 'daily'
        ? '0 9 * * *'
        : value === 'weekly'
          ? '0 9 * * 1'
          : (form.value.cron ?? '0 9 * * 1')
  occurrences.value = []
})

async function preview() {
  error.value = ''
  try {
    occurrences.value = (
      await api('/schedule/preview', {
        method: 'POST',
        body: JSON.stringify(form.value),
      })
    ).occurrences
  }
  catch (e) {
    error.value = (e as Error).message
  }
}

async function save() {
  busy.value = true
  error.value = ''
  try {
    const saved = await api<Task>(`/tasks${props.task ? `/${props.task.id}` : ''}`, {
      method: props.task ? 'PUT' : 'POST',
      body: JSON.stringify(form.value),
    })
    await refresh()
    emit('saved', saved)
    notify(props.task ? 'Task updated' : 'Task created')
    emit('close')
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
  <Modal
    :title="task ? 'Edit mission' : 'Give your agent a mission'"
    wide
    @close="emit('close')"
  >
    <form @submit.prevent="save">
      <div class="modal-body form-grid grid grid-cols-[1fr_1fr] gap-5 phone:grid-cols-1 phone:gap-4.5 px-6.5 py-6 phone:p-5">
        <label class="span-2 col-span-2 phone:col-span-1">Mission name<input
          v-model="form.name"
          required
          maxlength="100"
          autofocus
          placeholder="e.g. Keep CSS Baseline up to date"
        ></label><label class="span-2 col-span-2 phone:col-span-1">What should happen?<textarea
          v-model="form.prompt"
          required
          rows="5"
          placeholder="Describe the outcome, constraints, and how your agent should verify its work."
        /><small>Be specific about whether the agent may push, merge, or
          release.</small></label>
        <VirtualSelect
          v-model="form.agentId"
          class="span-2 col-span-2 phone:col-span-1"
          label="Agent"
          :options="agents"
          :icon="Bot"
          placeholder="Choose an agent"
          empty-text="Add an agent to get started"
          required
        />
        <p v-if="selectedAgent" class="inline-note bg-surface rounded-lg text-xs leading-[1.7] text-muted span-2 col-span-2 phone:col-span-1 agent-scope-summary px-[15px] py-[13px]">
          <Icon :name="Bot" :size="17" /><span>{{ scopeDescription }} · {{ selectedAgent.access.skills === null ? 'All available skills' : `${selectedAgent.access.skills.length} selected ${selectedAgent.access.skills.length === 1 ? 'skill' : 'skills'}` }} · {{ selectedAgent.access.sandbox === 'yolo' ? 'YOLO' : selectedAgent.access.sandbox }}</span>
        </p>
        <p
          v-if="!state.agents.length"
          class="inline-note bg-surface rounded-lg text-xs leading-[1.7] text-muted span-2 col-span-2 phone:col-span-1 px-[15px] py-[13px]"
        >
          Add an agent before creating your first task.
        </p>
        <div class="form-divider h-[1px] bg-line span-2 col-span-2 phone:col-span-1" />
        <label class="span-2 col-span-2 phone:col-span-1">Tags<input
          v-model="tagText"
          placeholder="maintenance, release"
        ><small>Separate tags with commas, up to ten.</small></label>
        <VirtualSelect v-model="cadence" label="When" :options="cadences" /><label v-if="cadence !== 'once'">Timezone<input
          v-model="form.timezone"
          required
          placeholder="Europe/Paris"
        ></label><label v-if="cadence === 'custom'" class="span-2 col-span-2 phone:col-span-1">Cron expression<input
          v-model="form.cron"
          placeholder="0 9 * * 1"
          required
        ><small>Minute · hour · day · month · weekday</small></label>
        <div v-if="cadence !== 'once'" class="span-2 col-span-2 phone:col-span-1 schedule-preview flex items-start gap-2 flex-wrap">
          <UiButton type="button" size="small" @click="preview">
            Preview next runs
          </UiButton><span v-for="time in occurrences" :key="time">{{
            new Intl.DateTimeFormat(undefined, {
              timeZone: form.timezone,
              dateStyle: "medium",
              timeStyle: "short",
            }).format(time)
          }}</span>
        </div>
        <label class="checkbox flex-row items-center gap-2 text-xs font-normal phone:text-xs phone:leading-[1.6] span-2 col-span-2 phone:col-span-1 mx-0 my-[9px]"><input v-model="advanced" type="checkbox">Customize mission scope</label>
        <template v-if="advanced">
          <VirtualSelect
            v-model="projectChoice"
            class="span-2 col-span-2 phone:col-span-1"
            label="Project context"
            :options="projects"
            :icon="FolderGit2"
          />
          <label class="checkbox flex-row items-center gap-2 text-xs font-normal phone:text-xs phone:leading-[1.6] span-2 col-span-2 phone:col-span-1 mx-0 my-[9px]"><input v-model="inheritedSkills" type="checkbox">Use the agent’s available skills</label>
          <fieldset v-if="!inheritedSkills && available.length" class="span-2 col-span-2 phone:col-span-1">
            <legend>
              Skills <small>Optional instructions your agent can reuse</small>
            </legend>
            <div class="check-grid flex gap-3 flex-wrap">
              <label
                v-for="skill in available"
                :key="`${skill.scope}/${skill.name}`"
                class="checkbox flex-row items-center gap-2 text-xs font-normal phone:text-xs phone:leading-[1.6] mx-0 my-[9px]"
              ><input
                v-model="form.skills"
                type="checkbox"
                :value="`${skill.scope}/${skill.name}`"
              >{{ skill.name }}</label>
            </div>
          </fieldset>
        </template>
        <UiAlert v-if="error" class="col-span-2 phone:col-span-1">
          {{ error }}
        </UiAlert>
      </div>
      <footer class="modal-actions flex justify-end gap-2.5 bg-surface border-t border-line sticky bottom-0 phone:flex-wrap px-6.5 py-4.5 phone:px-5 phone:py-4">
        <UiButton type="button" @click="emit('close')">
          Cancel
        </UiButton><UiButton
          variant="primary"
          type="submit"
          :disabled="busy || !state.agents.length"
        >
          {{ busy ? "Saving…" : task ? "Save changes" : "Create mission" }}
        </UiButton>
      </footer>
    </form>
  </Modal>
</template>

<style scoped>
.agent-scope-summary { display: flex; align-items: center; gap: .65rem; }
.agent-scope-summary svg { flex-shrink: 0; }
</style>
