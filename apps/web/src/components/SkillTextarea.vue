<script setup lang="ts">
import type { Mention, SkillOption } from '../skill-mentions'
import {
  computed,
  nextTick,
  ref,
  useId,
  watch,
} from 'vue'
import { state } from '../api'
import { BookOpen } from '../icons'
import {
  insertSkill,
  matchSkills,
  mentionAt,
  mentionSegments,
} from '../skill-mentions'
import Icon from './Icon.vue'

defineOptions({ inheritAttrs: false })
const props = defineProps<{ skills: SkillOption[] }>()
const emit = defineEmits<{ keydown: [KeyboardEvent] }>()
const model = defineModel<string>({ required: true })
const textarea = ref<HTMLTextAreaElement>()
const caret = ref(0)
const scrollTop = ref(0)
const active = ref(0)
const dismissed = ref<number | null>(null)
const listId = useId()
const names = computed(() => new Set(props.skills.map(skill => skill.name)))
const segments = computed(() => mentionSegments(model.value, names.value))
const highlighted = computed(() => segments.value.some(segment => segment.skill))
const mention = computed<Mention | null>(() => mentionAt(model.value, caret.value))
const matches = computed(() => mention.value ? matchSkills(props.skills, mention.value.query) : [])
const open = computed(() => !!mention.value && dismissed.value !== mention.value.start && (matches.value.length > 0 || !/^\d/.test(mention.value.query)))
watch(() => mention.value?.query, () => {
  active.value = 0
})
watch(() => mention.value?.start, (start) => {
  if (start !== dismissed.value)
    dismissed.value = null
})

function track() {
  caret.value = textarea.value?.selectionStart ?? model.value.length
  scrollTop.value = textarea.value?.scrollTop ?? 0
}

function focus() {
  dismissed.value = null
  track()
  // WebKit restores the selection after dispatching focus.
  requestAnimationFrame(() => {
    if (document.activeElement === textarea.value)
      track()
  })
}

function scope(skill: SkillOption) {
  return skill.scope === 'global' ? 'Global' : state.projects.find(project => project.id === skill.scope)?.name ?? 'Project'
}

function parts(name: string) {
  const query = mention.value?.query ?? ''
  const index = query ? name.indexOf(query) : -1
  return index < 0 ? [name, '', ''] : [name.slice(0, index), query, name.slice(index + query.length)]
}

async function choose(skill: SkillOption) {
  if (!mention.value)
    return
  const next = insertSkill(model.value, mention.value, skill.name)
  model.value = next.text
  await nextTick()
  textarea.value?.focus()
  textarea.value?.setSelectionRange(next.caret, next.caret)
  track()
}

function key(event: KeyboardEvent) {
  if (open.value && !event.isComposing) {
    const count = matches.value.length
    if (count > 0 && (event.key === 'ArrowDown' || event.key === 'ArrowUp')) {
      event.preventDefault()
      active.value = (active.value + (event.key === 'ArrowDown' ? 1 : count - 1)) % count
      document.getElementById(`${listId}-${active.value}`)?.scrollIntoView({ block: 'nearest' })
      return
    }

    if (count > 0 && ((event.key === 'Enter' && !event.shiftKey) || (event.key === 'Tab' && !event.shiftKey))) {
      event.preventDefault()
      void choose(matches.value[active.value]!)
      return
    }

    if (event.key === 'Escape') {
      event.preventDefault()
      event.stopPropagation()
      dismissed.value = mention.value!.start
      return
    }
  }

  emit('keydown', event)
}

defineExpose({ focus: () => textarea.value?.focus() })
</script>

<template>
  <div class="relative">
    <div
      v-if="open"
      :id="listId"
      :role="matches.length ? 'listbox' : 'status'"
      aria-label="Skills"
      class="skill-menu absolute inset-x-0 bottom-full z-20 mb-6 overflow-hidden rounded-xl border border-line bg-raised shadow-[0_12px_40px_#0000001f] phone:mb-5"
    >
      <div class="flex items-center justify-between gap-3 border-b border-line/70 px-3 py-2 text-[11px] text-muted">
        <span class="font-semibold uppercase tracking-wide">Skills</span>
        <span v-if="matches.length" class="phone:hidden"><kbd>↑</kbd><kbd>↓</kbd> navigate · <kbd>↵</kbd> insert · <kbd>Esc</kbd> dismiss</span>
      </div>
      <div v-if="!matches.length" class="px-4 py-3 text-sm">
        <p class="font-medium">
          {{ skills.length ? 'No matching skills' : 'No skills available for this conversation' }}
        </p>
        <p class="mt-1 text-xs text-muted">
          {{ skills.length ? 'Try another skill name.' : 'Manage global and project skills in the Skills library.' }}
        </p>
      </div>
      <ul v-else class="m-0 max-h-64 list-none overflow-auto p-1">
        <li
          v-for="(skill, index) in matches"
          :id="`${listId}-${index}`"
          :key="skill.name"
          role="option"
          :aria-selected="index === active"
          class="flex min-h-12 cursor-pointer items-center gap-3 rounded-lg px-3 py-2"
          :class="index === active ? 'bg-accent/10' : ''"
          @mousedown.prevent
          @mousemove="active = index"
          @click="choose(skill)"
        >
          <span class="flex size-7 shrink-0 items-center justify-center rounded-md bg-accent/10 text-accent"><Icon :name="BookOpen" :size="15" /></span>
          <span class="min-w-0 flex-1">
            <span class="flex min-w-0 items-center gap-2">
              <span class="truncate font-mono text-[13px] font-semibold"><span class="text-muted">$</span>{{ parts(skill.name)[0] }}<mark class="bg-transparent text-accent">{{ parts(skill.name)[1] }}</mark>{{ parts(skill.name)[2] }}</span>
              <span class="shrink-0 rounded bg-soft px-1.5 py-0.5 text-[10px] text-muted">{{ scope(skill) }}</span>
            </span>
            <span v-if="skill.description" class="block truncate text-xs text-muted">{{ skill.description }}</span>
          </span>
        </li>
      </ul>
    </div>
    <div v-if="highlighted" aria-hidden="true" class="skill-mirror pointer-events-none absolute inset-0 overflow-hidden text-sm text-transparent phone:text-[16px]">
      <div :style="{ transform: `translateY(${-scrollTop}px)` }">
        <template v-for="(segment, index) in segments" :key="index">
          <mark v-if="segment.skill" class="skill-token" v-text="segment.text" /><span v-else v-text="segment.text" />
        </template>
      </div>
    </div>
    <textarea
      ref="textarea"
      v-model="model"
      v-bind="$attrs"
      aria-autocomplete="list"
      :aria-controls="open ? listId : undefined"
      :aria-activedescendant="open && matches.length ? `${listId}-${active}` : undefined"
      class="relative block max-h-40 min-h-14 w-full resize-none border-0! bg-transparent! p-0! text-sm! phone:text-[16px]! shadow-none! outline-none! focus:ring-0!"
      @keydown="key"
      @input="track"
      @click="track"
      @keyup="track"
      @focus="focus"
      @select="track"
      @scroll="track"
      @blur="dismissed = mention?.start ?? null"
    />
  </div>
</template>

<style scoped>
.skill-mirror { white-space: pre-wrap; overflow-wrap: break-word; }
.skill-token {
  color: transparent;
  background: color-mix(in srgb, var(--color-accent) 16%, transparent);
  border-radius: 4px;
  box-shadow: 0 0 0 2px color-mix(in srgb, var(--color-accent) 16%, transparent);
}
kbd { font: inherit; }
</style>
