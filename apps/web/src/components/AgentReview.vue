<script setup lang="ts">
import type { ChipTone, DelegationEffect, ReviewLine } from '../agent-access'
import { ref } from 'vue'
import AccessChip from './AccessChip.vue'
import Modal from './Modal.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'

// What saving grants, takes away or suspends, confirmed before the agent is saved.
const props = defineProps<{
  lines: ReviewLine[]
  delegations: DelegationEffect[]
  busy: boolean
  error: string
}>()
const emit = defineEmits<{ close: [], confirm: [reapprove: string[]] }>()
// Reapproval is the default; a delegation that cannot be reapproved stops.
const reapprove = ref(props.delegations.filter(item => !item.blocked).map(item => item.id))
const kinds: Record<ReviewLine['kind'], [ChipTone, string]> = {
  power: ['power', 'Power'],
  secret: ['granted', 'Access'],
  reduction: ['unavailable', 'Reduction'],
}
</script>

<template>
  <Modal title="Review before saving" @close="!busy && emit('close')">
    <div class="grid gap-4 px-6.5 py-6 phone:p-5">
      <ul class="grid gap-3">
        <li v-for="line in lines" :key="line.text" class="flex items-start gap-2.5 text-sm leading-relaxed">
          <AccessChip :tone="kinds[line.kind][0]" class="mt-0.5">
            {{ kinds[line.kind][1] }}
          </AccessChip>
          <span class="min-w-0">{{ line.text }}</span>
        </li>
      </ul>
      <fieldset v-if="delegations.length" class="grid gap-2.5 border-t border-line pt-4">
        <legend class="mb-1 text-xs font-semibold">
          External effect
        </legend>
        <template v-for="delegation in delegations" :key="delegation.id">
          <p v-if="delegation.blocked" class="rounded-lg bg-warning-surface px-3.5 py-2.5 text-xs leading-relaxed text-warning">
            {{ delegation.name }} cannot answer as this agent with the new access: {{ delegation.blocked }} The bot will stop until you change it on its instance.
          </p>
          <label v-else class="flex flex-row! items-start gap-2.5! text-sm font-normal! text-ink!">
            <input
              v-model="reapprove"
              type="checkbox"
              class="mt-1 size-4 w-4!"
              :value="delegation.id"
            >
            <span>Reapprove the new access for {{ delegation.name }}<small class="block text-2xs">Unchecked, the bot stops answering until it is reapproved on its instance.</small></span>
          </label>
        </template>
      </fieldset>
      <UiAlert v-if="error">
        {{ error }}
      </UiAlert>
    </div>
    <footer class="modal-actions sticky bottom-0 flex justify-end gap-2.5 border-t border-line bg-surface px-6.5 py-4.5 phone:flex-wrap phone:px-5 phone:py-4">
      <UiButton :disabled="busy" @click="emit('close')">
        Back to editing
      </UiButton>
      <UiButton variant="primary" :disabled="busy" @click="emit('confirm', reapprove)">
        {{ busy ? 'Saving…' : 'Save' }}
      </UiButton>
    </footer>
  </Modal>
</template>
