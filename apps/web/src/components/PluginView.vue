<script setup lang="ts">
import { ArrowUpRight } from '../icons'
import Icon from './Icon.vue'
import PluginViewForm from './PluginViewForm.vue'
import UiButton from './UiButton.vue'

// A package's declarative view: components validated by the server, rendered natively.
// Buttons and forms ask the package to run an action.
defineProps<{ nodes: any[], busy?: boolean }>()
const emit = defineEmits<{ action: [action: string, payload: unknown, values?: Record<string, any>] }>()

const tones: Record<string, string> = {
  neutral: 'bg-hover text-muted',
  info: 'bg-soft text-accent',
  success: 'bg-[light-dark(#dff3ee,#163a33)] text-success',
  warning: 'bg-warning-surface text-warning',
  danger: 'bg-coral-soft text-coral',
}
const variants: Record<string, 'primary' | 'danger-outline' | undefined> = {
  primary: 'primary',
  danger: 'danger-outline',
}

function press(button: { action: string, payload?: unknown, confirm?: string }) {
  // eslint-disable-next-line no-alert
  if (button.confirm && !window.confirm(button.confirm))
    return
  emit('action', button.action, button.payload ?? null)
}
</script>

<template>
  <template v-for="(node, index) in nodes" :key="index">
    <section v-if="node.type === 'section'" class="grid gap-3">
      <div v-if="node.title || node.description">
        <h3 v-if="node.title" class="text-xs! font-semibold">
          {{ node.title }}
        </h3>
        <p v-if="node.description" class="mt-0.5 text-2xs leading-relaxed text-muted">
          {{ node.description }}
        </p>
      </div>
      <PluginView :nodes="node.children" :busy="busy" @action="(...args) => emit('action', ...args)" />
    </section>
    <p v-else-if="node.type === 'text'" class="whitespace-pre-line text-xs leading-relaxed" :class="node.tone && node.tone !== 'neutral' ? 'text-muted' : ''">
      {{ node.text }}
    </p>
    <div
      v-else-if="node.type === 'alert'"
      class="rounded-xl px-3.5 py-2.5 text-xs leading-relaxed"
      :class="tones[node.tone]"
      role="status"
    >
      <b v-if="node.title" class="block">{{ node.title }}</b>{{ node.text }}
    </div>
    <span v-else-if="node.type === 'badge'" class="inline-flex h-5.5 w-fit items-center rounded-full px-2.25 text-2xs font-semibold" :class="tones[node.tone ?? 'neutral']">
      {{ node.label }}
    </span>
    <dl v-else-if="node.type === 'facts'" class="grid grid-cols-[repeat(auto-fill,minmax(160px,1fr))] gap-2.5">
      <div v-for="item in node.items" :key="item.label" class="rounded-xl border border-line bg-inset px-3.5 py-2.5">
        <dt class="text-3xs font-semibold uppercase tracking-[.12em] text-subtle">
          {{ item.label }}
        </dt>
        <dd class="mt-1 truncate text-xs font-semibold">
          {{ item.value ?? '—' }}
        </dd>
      </div>
    </dl>
    <div v-else-if="node.type === 'list'">
      <p v-if="!node.items.length" class="text-xs text-muted">
        {{ node.empty || 'Nothing yet.' }}
      </p>
      <ul v-else class="overflow-hidden rounded-xl border border-line">
        <li v-for="(item, position) in node.items" :key="position" class="flex items-center gap-3 border-t border-line px-3.5 py-2.5 first:border-0 phone:flex-wrap">
          <span class="min-w-0 flex-1">
            <span class="block truncate text-xs font-semibold">{{ item.title }}</span>
            <span v-if="item.subtitle" class="block truncate text-2xs text-muted">{{ item.subtitle }}</span>
          </span>
          <span v-if="item.badge" class="inline-flex h-5.5 items-center rounded-full px-2.25 text-2xs font-semibold" :class="tones[item.badge.tone ?? 'neutral']">{{ item.badge.label }}</span>
          <UiButton
            v-for="button in item.actions ?? []"
            :key="button.label"
            size="small"
            :variant="variants[button.style]"
            :disabled="busy"
            @click="press(button)"
          >
            {{ button.label }}
          </UiButton>
        </li>
      </ul>
    </div>
    <div v-else-if="node.type === 'progress'" class="grid gap-1.5">
      <div class="flex justify-between text-2xs">
        <span class="text-muted">{{ node.label }}</span><span>{{ node.value }} / {{ node.max }}</span>
      </div>
      <div
        class="h-1.5 overflow-hidden rounded-full bg-hover"
        role="meter"
        :aria-label="node.label || 'Progress'"
        :aria-valuenow="node.value"
        aria-valuemin="0"
        :aria-valuemax="node.max"
      >
        <span class="block h-full rounded-full bg-accent" :style="{ width: `${Math.min(100, node.value / node.max * 100)}%` }" />
      </div>
    </div>
    <pre v-else-if="node.type === 'code'" class="overflow-x-auto rounded-xl border border-line bg-inset px-3 py-2.5 font-mono text-3xs leading-relaxed"><code>{{ node.text }}</code></pre>
    <a
      v-else-if="node.type === 'link'"
      :href="node.url"
      target="_blank"
      rel="noopener noreferrer"
      class="inline-flex w-fit items-center gap-1 text-xs font-semibold text-accent"
    >
      {{ node.label }}<Icon :name="ArrowUpRight" :size="12" />
    </a>
    <div v-else-if="node.type === 'actions'" class="flex flex-wrap gap-2">
      <UiButton
        v-for="button in node.children"
        :key="button.label"
        size="small"
        :variant="variants[button.style]"
        :disabled="busy"
        @click="press(button)"
      >
        {{ button.label }}
      </UiButton>
    </div>
    <UiButton
      v-else-if="node.type === 'button'"
      size="small"
      class="w-fit"
      :variant="variants[node.style]"
      :disabled="busy"
      @click="press(node)"
    >
      {{ node.label }}
    </UiButton>
    <PluginViewForm
      v-else-if="node.type === 'form'"
      :node="node"
      :busy="busy"
      @submit="(action, values) => emit('action', action, null, values)"
    />
  </template>
</template>
