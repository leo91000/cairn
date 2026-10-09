<script setup lang="ts">
import type { Run } from '../../../../packages/contracts/contracts'
import { duration } from '../api'
import UiButton from './UiButton.vue'

defineProps<{ run: Run, active: boolean }>()
const emit = defineEmits<{ cleanup: [] }>()
</script>

<template>
  <div class="run-details">
    <div class="run-facts grid grid-cols-[repeat(4,_1fr)] border border-line bg-raised rounded-[10px] text-xs text-subtle phone:grid-cols-2 phone:gap-5 px-6 py-5 mx-0 my-6.5">
      <span>Project<strong>{{ run.snapshot.projects?.map(project => project.name).join(', ') || run.snapshot.project?.name || 'Agent workspace' }}</strong></span><span>Duration<strong>{{ duration(run.startedAt, run.finishedAt) }}</strong></span><span>Triggered by<strong>{{ run.trigger }}</strong></span><span>Model<strong>{{ run.snapshot.agent.model || 'Codex default' }}</strong></span>
    </div>
    <h3>Original instructions</h3>
    <pre class="brief-text whitespace-pre-wrap text-xs leading-[1.9] text-muted">{{ run.snapshot.task.prompt }}</pre>
    <h3>Workspace</h3>
    <code>{{
      run.workspace
        || (run.workspaceCleanedAt ? "Worktree cleaned up" : "Not prepared yet")
    }}</code>
    <p v-for="entry in run.workspaces?.filter(entry => entry.revision)" :key="entry.projectId" class="text-xs text-muted">
      Starting commit <code :title="entry.revision || undefined">{{ entry.revision?.slice(0, 12) }}</code>
    </p>
    <UiButton
      v-if="!active && run.workspace && run.snapshot.task.worktree && !run.isolated"
      size="small"
      @click="emit('cleanup')"
    >
      Clean up worktree
    </UiButton>
    <p v-if="!active && run.isolated && run.snapshot.task.worktree" class="muted text-muted">
      Workspace and conversation are saved in a private VM disk. Resume this run to continue working.
    </p>
    <h3>Selected skills</h3>
    <p>
      {{
        run.snapshot.skills.map((s) => s.name).join(", ")
          || "No skills selected"
      }}
    </p>
    <h3>Execution access</h3>
    <p>
      {{ run.snapshot.agent.access?.sandbox === 'workspace-write' ? 'Workspace write' : run.snapshot.agent.access?.sandbox === 'read-only' ? 'Read only' : 'YOLO mode' }} ·
      {{ run.isolated ? 'Isolated container' : 'Shared workspace' }} ·
      {{ run.snapshot.agent.timeoutMinutes === 0 ? 'No time limit' : `${run.snapshot.agent.timeoutMinutes} minute limit` }}
    </p>
  </div>
</template>
