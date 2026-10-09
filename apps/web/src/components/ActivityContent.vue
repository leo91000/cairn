<script setup lang="ts">
import { computed, ref } from 'vue'
import { contentParts, dataSummary, dataTitle } from '../activity-data'
import { Braces, ChevronDown, Info } from '../icons'
import ActivityCode from './ActivityCode.vue'
import ActivityDataNode from './ActivityDataNode.vue'
import Icon from './Icon.vue'
import Markdown from './Markdown.vue'

const props = defineProps<{ content: string, label?: string, language?: string }>()
const sourceOpen = ref(new Set<number>())

function toggleSource(index: number, event: Event) {
  if ((event.target as HTMLDetailsElement).open)
    sourceOpen.value.add(index)
  else
    sourceOpen.value.delete(index)
}

const parts = computed(() => props.language && !['json', 'plaintext'].includes(props.language) ? [{ kind: 'text' as const, text: props.content }] : contentParts(props.content))
</script>

<template>
  <template v-for="(part, index) in parts" :key="index">
    <section v-if="part.kind === 'data'" class="activity-data border border-line rounded-[14px] overflow-hidden bg-raised min-w-0 text-ink [box-shadow:0_3px_12px_light-dark(#26243c05,_#00000005)] mx-0 my-4" :aria-label="dataTitle(part.value)">
      <header class="data-header flex items-center gap-[11px] bg-surface [border-bottom:1px_solid_light-dark(#dcdbe9,_var(--dark-border))] px-4.5 py-4 phone:p-[13px]">
        <span class="data-header-icon grid place-items-center w-[35px] h-[35px] rounded-[10px] text-muted bg-raised border border-line shrink-0"><Icon :name="Braces" :size="18" /></span><div><strong>{{ dataTitle(part.value) }}</strong><small>{{ label || 'Agent output' }} · {{ dataSummary(part.value) }}</small></div>
      </header>
      <div class="data-content pt-1 pb-3.5 overflow-visible overscroll-contain [scrollbar-width:thin] phone:pt-[3px] phone:pb-3 px-4.5 phone:px-3">
        <ActivityDataNode :value="part.value" />
      </div>
      <details class="data-source [border-top:1px_solid_light-dark(#deddea,_var(--dark-border))] bg-surface" @toggle="toggleSource(index, $event)">
        <summary><Icon :name="Braces" :size="14" />View JSON<Icon :name="ChevronDown" :size="14" /></summary><ActivityCode
          v-if="sourceOpen.has(index)"
          label="JSON"
          :code="JSON.stringify(part.value, null, 2)"
          :copy-code="part.source"
          language="json"
        />
      </details>
    </section>
    <section v-else-if="part.kind === 'incomplete'" class="activity-data border border-line rounded-[14px] overflow-hidden bg-raised min-w-0 text-ink [box-shadow:0_3px_12px_light-dark(#26243c05,_#00000005)] mx-0 my-4" aria-label="Incomplete result">
      <header class="data-header flex items-center gap-[11px] bg-surface [border-bottom:1px_solid_light-dark(#dcdbe9,_var(--dark-border))] px-4.5 py-4 phone:p-[13px]">
        <span class="data-header-icon grid place-items-center w-[35px] h-[35px] rounded-[10px] text-muted bg-raised border border-line shrink-0"><Icon :name="Info" :size="18" /></span><div><strong>Incomplete result</strong><small>{{ label || 'Agent output' }} · Partial JSON</small></div>
      </header>
      <div class="data-content pt-1 pb-3.5 overflow-visible overscroll-contain [scrollbar-width:thin] phone:pt-[3px] phone:pb-3 px-4.5 phone:px-3">
        <p class="artifact-no-output text-2xs text-muted leading-[1.8]">
          The saved output ends partway through this result. A complete preview isn’t available.
        </p>
      </div>
      <details class="data-source [border-top:1px_solid_light-dark(#deddea,_var(--dark-border))] bg-surface" @toggle="toggleSource(index, $event)">
        <summary><Icon :name="Braces" :size="14" />View saved source<Icon :name="ChevronDown" :size="14" /></summary>
        <ActivityCode
          v-if="sourceOpen.has(index)"
          label="Saved source"
          :code="part.source"
          language="json"
        />
      </details>
    </section>
    <template v-else-if="part.text.trim()">
      <ActivityCode
        v-if="label"
        :label="label"
        :code="part.text"
        :language="language"
      />
      <Markdown v-else :content="part.text" />
    </template>
  </template>
</template>
