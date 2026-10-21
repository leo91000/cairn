<script setup lang="ts">
import type { FilItem } from '../signal'
import { computed } from 'vue'
import { restoreOutcome, setAside } from '../dismissals'
import { Check, Clock, Undo2 } from '../icons'
import { dismissible, tomorrowMorning } from '../signal'
import Icon from './Icon.vue'
import UiButton from './UiButton.vue'

// Under a blocked mission's outcome: who it waits on, and what the signed-in account can do about
// it. The item comes from the Fil, so both always agree on where the mission sits.
const props = defineProps<{ item: FilItem }>()
const outcome = computed(() => ({ id: props.item.runId!, taskId: props.item.taskId!, finishedAt: props.item.at }))
const snoozedUntil = computed(() => props.item.wakeAt ? new Date(props.item.wakeAt).toLocaleString([], { weekday: 'long', hour: '2-digit', minute: '2-digit' }) : '')
</script>

<template>
  <div class="mt-3 flex flex-wrap items-center gap-2 border-t border-warning/20 pt-3 text-[12px] text-muted" data-testid="mission-outcome-actions">
    <template v-if="item.setAside === 'requester'">
      <p class="m-0!">
        Waiting on <RouterLink :to="item.requester!.to" class="font-semibold text-accent">
          {{ item.requester!.title }}
        </RouterLink>, which started this mission. Nothing to do on your side.
      </p>
    </template>
    <p v-else-if="item.setAside === 'external'" class="m-0!">
      Waiting on an outside check, such as CI or a review. Nothing to do on your side.
    </p>
    <template v-else-if="item.setAside === 'dismissed' || item.kind === 'snoozed'">
      <p class="m-0! mr-auto">
        {{ item.kind === 'snoozed' ? `Snoozed until ${snoozedUntil}.` : 'Dismissed: it stays out of For you until the mission reports again.' }}
      </p>
      <UiButton size="small" @click="restoreOutcome(outcome)">
        <Icon :name="Undo2" :size="14" />Back to For you
      </UiButton>
    </template>
    <template v-else>
      <UiButton
        v-if="dismissible(item)"
        size="small"
        title="Nothing to do on your side: it leaves For you until the mission reports again"
        @click="setAside(outcome)"
      >
        <Icon :name="Check" :size="14" />Dismiss
      </UiButton>
      <UiButton size="small" @click="setAside(outcome, tomorrowMorning())">
        <Icon :name="Clock" :size="14" />Remind me tomorrow
      </UiButton>
    </template>
  </div>
</template>
