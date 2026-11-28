<script setup lang="ts">
import type { HookRule } from '../../../../packages/contracts/generated/typescript/HookRule'
import type { HookRuleCatalog } from '../../../../packages/contracts/generated/typescript/HookRuleCatalog'
import type { HookRuleTrigger } from '../../../../packages/contracts/generated/typescript/HookRuleTrigger'
import {
  computed,
  ref,
  toRaw,
  watch,
} from 'vue'
import { createHookRule } from '../../../../packages/contracts/generated/createHookRule'
import { replaceHookRule } from '../../../../packages/contracts/generated/replaceHookRule'
import { api, state } from '../api'
import { sameRuleDefinition } from '../hook-rules'
import ClockScheduleFields from './ClockScheduleFields.vue'
import Modal from './Modal.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'
import VirtualSelect from './VirtualSelect.vue'

const props = defineProps<{ catalog: HookRuleCatalog, preferredAgent?: string }>()
const emit = defineEmits<{ created: [rule: HookRule], close: [] }>()
const agents = props.catalog.agents
const agent = ref(agents.find(item => item.agentId === props.preferredAgent)?.agentId ?? agents[0]?.agentId ?? '')
const project = ref('')
const name = ref('')
const brief = ref('')
const trigger = ref<Extract<HookRuleTrigger, { type: 'clock' }>>({
  type: 'clock',
  cron: '0 9 * * *',
  timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC',
})
const projects = computed(() => {
  const allowed = state.agents.find(item => item.id === agent.value)?.access.projects
  return [{ label: 'Agent workspace', value: '' }, ...state.projects
    .filter(item => allowed == null || allowed.includes(item.id))
    .map(item => ({ label: item.name, value: item.id }))]
})

watch(projects, (options) => {
  if (!options.some(option => option.value === project.value))
    project.value = ''
})
const saving = ref(false)
const error = ref('')
const creationId = crypto.randomUUID()
const ready = computed(() => agent.value && name.value.trim() && brief.value.trim()
  && (trigger.value.cron || (trigger.value.at != null && trigger.value.at > Date.now())))

async function save() {
  if (!ready.value || saving.value)
    return

  saving.value = true
  error.value = ''
  try {
    const definition = {
      name: name.value.trim(),
      description: '',
      trigger: toRaw(trigger.value),
      executionAgentId: agent.value,
      limits: props.catalog.limits.defaultLimits,
      steps: [{
        id: 'avoid_overlap',
        type: 'if' as const,
        condition: { type: 'not' as const, condition: { type: 'ruleConversationWorking' as const } },
        then: [{
          id: 'start_agent',
          type: 'action' as const,
          action: {
            type: 'startAgent' as const,
            agentId: agent.value,
            projectId: project.value || null,
            prompt: { type: 'literal' as const, value: brief.value.trim() },
          },
        }],
        otherwise: [],
      }],
    }
    // The same form keeps its creation identity after a lost response, including
    // when the user has made corrections before retrying.
    let rule = await createHookRule(api, { creationId, definition, enabled: true })
    if (!sameRuleDefinition(rule.definition, definition)) {
      if (rule.version !== 1)
        throw new Error('This schedule was changed elsewhere. Open it from Rules to continue editing.')

      rule = await replaceHookRule(api, { ruleId: rule.id, revision: rule.revision, definition })
    }

    emit('created', rule)
  }
  catch (e) {
    error.value = (e as Error).message || 'Could not confirm the schedule was saved. Try again to recover it.'
  }
  finally {
    saving.value = false
  }
}
</script>

<template>
  <Modal title="Schedule an agent" @close="!saving && emit('close')">
    <form class="grid gap-4 p-5" @submit.prevent="save">
      <UiAlert v-if="error">
        {{ error }}
      </UiAlert>
      <label>Name<input v-model="name" required maxlength="100"></label>
      <VirtualSelect
        v-model="agent"
        label="Agent"
        :options="agents.map(item => ({ label: item.name, value: item.agentId }))"
        required
      />
      <VirtualSelect
        v-model="project"
        label="Project"
        :options="projects"
        placeholder="Agent workspace"
      />
      <label>Brief<textarea v-model="brief" rows="4" required /></label>
      <ClockScheduleFields v-model="trigger" />
      <UiButton type="submit" variant="primary" :disabled="!ready || saving">
        Create schedule
      </UiButton>
    </form>
  </Modal>
</template>
