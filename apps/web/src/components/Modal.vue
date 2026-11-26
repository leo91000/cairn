<script setup lang="ts">
import { twMerge } from 'tailwind-merge'
import {
  nextTick,
  onBeforeUnmount,
  onMounted,
  ref,
  useId,
} from 'vue'
import { state } from '../api'
import { X } from '../icons'
import { iconButton } from '../ui'
import Icon from './Icon.vue'

// `drawer` opens a panel along the right edge, and a bottom sheet on phones like `sheet`.
// `headless` names the dialog without a title bar; the content draws its own heading.
// `workbench` fills most of the viewport for editors with their own layout, and all of it on phones.
const props = defineProps<{
  title: string
  wide?: boolean
  workbench?: boolean
  sheet?: boolean
  drawer?: boolean
  headless?: boolean
  returnFocus?: HTMLElement
}>()
const emit = defineEmits<{ close: [] }>()
const dialog = ref<HTMLDialogElement>()
const titleId = useId()
const previous = document.activeElement as HTMLElement | null
onMounted(async () => {
  await nextTick()
  dialog.value?.showModal()
})
onBeforeUnmount(() => {
  dialog.value?.close()
  const target = props.returnFocus ?? previous
  target?.focus()
})
</script>

<template>
  <Teleport to="body">
    <dialog
      ref="dialog"
      class="modal m-auto border border-line rounded-[15px] max-w-[min(560px,_calc(100vw_-_28px))] w-full max-h-[calc(100dvh_-_32px_-_env(safe-area-inset-top)_-_env(safe-area-inset-bottom))] bg-raised text-ink [box-shadow:0_25px_90px_light-dark(#12112335,_#00000035)] p-0"
      :aria-labelledby="headless ? undefined : titleId"
      :aria-label="headless ? title : undefined"
      :class="{ wide, drawer, workbench, 'mobile-sheet': sheet || drawer }"
      @cancel.prevent="emit('close')"
      @click="
        (e) => {
          if (e.target === dialog) emit('close');
        }
      "
    >
      <div class="modal-inner max-h-[calc(100dvh_-_34px_-_env(safe-area-inset-top)_-_env(safe-area-inset-bottom))] overflow-auto overscroll-contain">
        <div class="sticky top-0 z-2 shrink-0 bg-raised">
          <header v-if="!headless" class="modal-head flex items-center justify-between border-b border-line px-6.5 py-5.5 phone:p-[19px]">
            <h2 :id="titleId" class="min-w-0">
              <slot name="heading">
                {{ title }}
              </slot>
            </h2>
            <button
              :class="twMerge(iconButton, 'icon-button')"
              aria-label="Close dialog"
              @click="emit('close')"
            >
              <Icon :name="X" :size="20" />
            </button>
          </header>
          <p
            v-if="state.toast || state.connectionLost"
            role="status"
            aria-live="polite"
            class="m-0 border-b border-line px-6.5 py-2 text-xs text-muted phone:px-5"
          >
            {{ state.toast || 'Connection lost · reconnecting…' }}
          </p>
        </div>
        <slot />
      </div>
    </dialog>
  </Teleport>
</template>

<style scoped>
.drawer {
  margin: 12px 12px 12px auto;
  height: calc(100dvh - 24px);
  max-height: none;
  max-width: min(440px, calc(100vw - 24px));
  border-radius: 22px;
}
.drawer .modal-inner {
  height: 100%;
  max-height: none;
  display: flex;
  flex-direction: column;
}
.workbench {
  max-width: min(1320px, calc(100vw - 40px));
  height: min(900px, calc(100dvh - 40px));
  max-height: none;
  border-radius: 18px;
}
.workbench .modal-inner {
  display: flex;
  flex-direction: column;
  height: 100%;
  max-height: none;
  overflow: hidden;
}
@media (prefers-reduced-motion: no-preference) {
  .workbench[open] { animation: workbench-in 0.18s ease-out; }
}
@keyframes workbench-in {
  from { opacity: 0; }
}
@media (prefers-reduced-motion: no-preference) {
  .drawer[open] { animation: drawer-in 0.3s var(--ease-signal); }
}
@keyframes drawer-in {
  from { transform: translateX(28px); opacity: 0; }
}
@media (max-width: 640px) {
  .drawer { height: auto; }
  .drawer .modal-inner { height: auto; }
  .drawer[open] { animation-name: sheet-in; }
  @keyframes sheet-in {
    from { transform: translateY(40px); opacity: 0; }
  }
  .mobile-sheet {
    inset: auto 0 0;
    margin: 0;
    width: 100%;
    max-width: none;
    max-height: calc(100dvh - env(safe-area-inset-top) - 16px);
    border-radius: 22px 22px 0 0;
  }
  /* A wide sheet still spans the phone; `.modal.wide` would cap it and pin it to the left. */
  .mobile-sheet.wide { max-width: none; }
  .mobile-sheet .modal-inner {
    max-height: calc(100dvh - env(safe-area-inset-top) - 18px);
    padding-bottom: env(safe-area-inset-bottom);
  }
  .mobile-sheet .modal-head { padding: 16px 20px 8px; border-bottom: 0; }
  .workbench {
    inset: 0;
    margin: 0;
    width: 100%;
    max-width: none;
    height: 100dvh;
    border: 0;
    border-radius: 0;
    padding: env(safe-area-inset-top) 0 env(safe-area-inset-bottom);
  }
  .mobile-sheet::backdrop { background: #0a091080; backdrop-filter: blur(3px); }
}
</style>
