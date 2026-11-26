<script setup lang="ts">
import { Check, CircleAlert } from '../icons'
import AuthStage from './AuthStage.vue'
import Icon from './Icon.vue'
import UiAlert from './UiAlert.vue'

withDefaults(defineProps<{
  state: 'loading' | 'success' | 'error'
  title: string
  message?: string
  error?: string
  embedded?: boolean
}>(), {
  message: '',
  error: '',
  embedded: false,
})
</script>

<template>
  <AuthStage :embedded="embedded" :busy="state === 'loading'">
    <div class="flex flex-col items-center text-center">
      <div
        v-if="state === 'loading'"
        class="souffle-halo mb-5 size-16"
        aria-hidden="true"
      >
        <span class="souffle-core grid size-10 place-items-center rounded-xl border border-line bg-surface">
          <img class="cairn-mark-light size-6" src="/brand/cairn-symbol.svg" alt="">
          <img class="cairn-mark-dark size-6" src="/brand/cairn-symbol-dark.svg" alt="">
        </span>
      </div>
      <span
        v-else-if="state === 'success'"
        class="mb-5 grid size-14 place-items-center rounded-2xl border border-success/30 bg-success/12 text-success"
        aria-hidden="true"
      >
        <Icon :name="Check" :size="24" />
      </span>
      <span
        v-else
        class="mb-5 grid size-14 place-items-center rounded-2xl border border-danger/30 bg-danger-surface text-danger"
        aria-hidden="true"
      >
        <Icon :name="CircleAlert" :size="24" />
      </span>

      <h1 class="font-heading text-xl font-bold tracking-tight">
        {{ title }}
      </h1>

      <UiAlert v-if="error" class="mt-4 w-full text-left">
        {{ error }}
      </UiAlert>
      <p v-else-if="message" role="status" class="mt-2.5 text-sm text-muted">
        {{ message }}
      </p>

      <div v-if="$slots.actions" class="mt-6 flex w-full flex-col gap-2.5">
        <slot name="actions" />
      </div>
    </div>
  </AuthStage>
</template>
