<script setup lang="ts">
import { twMerge } from 'tailwind-merge'
import {
  onBeforeUnmount,
  onMounted,
  ref,
  watch,
} from 'vue'
import { api, date, duration } from '../api'
import Empty from '../components/Empty.vue'
import Icon from '../components/Icon.vue'
import Outcome from '../components/Outcome.vue'
import Status from '../components/Status.vue'
import UiAlert from '../components/UiAlert.vue'
import UiButton from '../components/UiButton.vue'
import VirtualSelect from '../components/VirtualSelect.vue'
import {
  ArrowUpRight,
  CircleCheck,
  CirclePause,
  Clock,
  ListFilter,
  LoaderCircle,
  RefreshCw,
  Square,
  TriangleAlert,
} from '../icons'
import { iconButton } from '../ui'

const runs = ref<any[]>([])
const status = ref('')
const error = ref('')
const offset = ref(0)
const busy = ref(false)
const outcomes = [
  { value: '', label: 'All executions', icon: ListFilter },
  {
    value: 'queued',
    label: 'Queued',
    icon: Clock,
    group: 'In progress',
  },
  {
    value: 'running',
    label: 'Running',
    icon: LoaderCircle,
    group: 'In progress',
  },
  {
    value: 'succeeded',
    label: 'Execution finished',
    icon: CircleCheck,
    group: 'Finished',
  },
  {
    value: 'failed',
    label: 'Failed',
    icon: TriangleAlert,
    group: 'Finished',
  },
  {
    value: 'cancelled',
    label: 'Cancelled',
    icon: Square,
    group: 'Finished',
  },
  {
    value: 'interrupted',
    label: 'Interrupted',
    icon: CirclePause,
    group: 'Finished',
  },
]

async function load() {
  busy.value = true
  try {
    runs.value = await api(
      `/runs?offset=${offset.value}&limit=30${status.value ? `&status=${status.value}` : ''}`,
    )
    error.value = ''
  }
  catch (e) {
    error.value = (e as Error).message
  }
  finally {
    busy.value = false
  }
}

watch(status, () => {
  offset.value = 0
  load()
})
watch(offset, load)
let timer: ReturnType<typeof setInterval>
onMounted(() => {
  load()
  timer = setInterval(() => {
    if (!document.hidden)
      load()
  }, 5000)
})
onBeforeUnmount(() => clearInterval(timer))
</script>

<template>
  <div class="page-heading flex items-center justify-between gap-5 mb-[27px] phone:gap-2.5 phone:flex-wrap phone:mb-[21px]">
    <div>
      <h1>Run history</h1>
    </div>
    <UiButton :disabled="busy" @click="load">
      <Icon :name="RefreshCw" :size="16" />Refresh
    </UiButton>
  </div>
  <div class="toolbar flex items-center justify-between gap-5 mb-[23px] tablet:items-start tablet:flex-wrap phone:gap-4 phone:min-w-0">
    <span class="muted text-muted">{{ runs.length }} runs on this page</span><div class="inline-label flex flex-row items-center gap-2.5 text-xs text-muted whitespace-nowrap min-w-0 max-w-full">
      <span>Status</span><VirtualSelect
        v-model="status"
        label="Status"
        :options="outcomes"
        compact
        hide-label
        clearable
      />
    </div>
  </div>
  <UiAlert v-if="error">
    {{ error }}
  </UiAlert>
  <div v-if="runs.length" class="panel border-t border-line overflow-hidden table-wrap overflow-auto relative">
    <table class="run-table phone:whitespace-normal">
      <thead>
        <tr>
          <th>Task</th>
          <th>Outcome</th>
          <th>Started</th>
          <th>Duration</th>
          <th>Trigger</th>
          <th><span class="sr-only">Details</span></th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="run in runs" :key="run.id">
          <td>
            <RouterLink :to="`/runs/${run.id}`" class="table-name font-[550] text-ink">
              {{ run.taskName }}
            </RouterLink><small>{{ run.agentName }}</small>
          </td>
          <td data-label="Outcome">
            <Status :status="run.status" /><Outcome :outcome="run.outcome" :status="run.status" />
          </td>
          <td data-label="Started">
            {{ date(run.createdAt) }}
          </td>
          <td data-label="Duration">
            {{ duration(run.startedAt, run.finishedAt) }}
          </td>
          <td data-label="Trigger">
            <span class="pill inline-flex bg-soft text-accent rounded-[5px] text-xs font-[650] whitespace-nowrap muted-pill bg-surface text-muted px-2 py-1">{{ run.trigger }}</span>
          </td>
          <td>
            <RouterLink
              :to="`/runs/${run.id}`"
              :class="twMerge(iconButton, 'icon-button')"
              :aria-label="`View ${run.taskName}`"
            >
              <Icon :name="ArrowUpRight" :size="18" />
            </RouterLink>
          </td>
        </tr>
      </tbody>
    </table>
  </div>
  <Empty
    v-else
    title="A clean slate"
    description="Once an agent starts working, you’ll find its progress and final result here."
  />
  <div v-if="offset || runs.length === 30" class="pagination flex items-center justify-end gap-4.5 text-xs text-muted mx-0 my-5">
    <UiButton

      :disabled="offset === 0"
      @click="offset = Math.max(0, offset - 30)"
    >
      Previous
    </UiButton><span>Page {{ offset / 30 + 1 }}</span><UiButton :disabled="runs.length < 30" @click="offset += 30">
      Next
    </UiButton>
  </div>
</template>
