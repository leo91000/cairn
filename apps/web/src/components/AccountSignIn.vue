<script setup lang="ts">
import type { Provider } from '../../../../packages/contracts/accounts'
import type { AccountsState } from '../accounts'
import { computed, ref, watch } from 'vue'
import { providers } from '../../../../packages/contracts/accounts'
import { brands } from '../accounts'
import {
  ArrowUpRight,
  Check,
  Copy,
  LoaderCircle,
} from '../icons'
import { buttonBase, buttonSizes, buttonVariants } from '../ui'
import Icon from './Icon.vue'
import Modal from './Modal.vue'
import ProviderMark from './ProviderMark.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'

// Adds an account or signs one in again, with the same steps for every coding agent: the
// official page opens in a new tab and this window follows until the account is verified.
const props = defineProps<{ accounts: AccountsState, provider?: Provider }>()
const emit = defineEmits<{ close: [] }>()
const chosen = ref<Provider>(props.provider ?? 'codex')
const name = ref('')
const code = ref('')
const submitted = ref(false)
const copied = ref(false)
const copyError = ref('')
const flow = computed(() => props.accounts.signIn.value?.state === 'pending' || props.accounts.signIn.value?.state === 'failed' ? props.accounts.signIn.value : null)
const busy = computed(() => props.accounts.busy.value)
const title = computed(() => {
  if (!flow.value)
    return 'Add an account'
  if (flow.value.state === 'failed')
    return 'Let’s try that again'
  return `Connect your ${providers[flow.value.provider].subscription}`
})
watch(() => props.accounts.signIn.value?.state, (state) => {
  if (state === 'complete')
    emit('close')
})
// Separate sources: each poll brings a new sign-in object, which must not reset the feedback.
watch([() => flow.value?.code, () => flow.value?.state], () => {
  copied.value = false
  copyError.value = ''
  submitted.value = false
})

async function add() {
  await props.accounts.add(chosen.value, name.value.trim() || `${providers[chosen.value].label} account`)
}

async function copyCode() {
  const value = flow.value?.code
  if (!value)
    return
  try {
    // Called directly from the click, without delaying the link's navigation.
    await navigator.clipboard.writeText(value)
    copied.value = true
    copyError.value = ''
  }
  catch {
    copied.value = false
    copyError.value = 'Couldn’t copy automatically. Select the code and copy it manually.'
  }
}

async function submit() {
  if (!code.value.trim())
    return
  await props.accounts.submitCode(code.value.trim())
  code.value = ''
  submitted.value = !props.accounts.error.value
}

async function close() {
  // Leaving before the account is verified cancels the sign-in; an account that never
  // connected is removed.
  if (flow.value)
    await props.accounts.cancel()
  emit('close')
}
</script>

<template>
  <Modal :title="title" sheet @close="close">
    <section class="grid gap-5 p-6 phone:px-5" :aria-label="title">
      <UiAlert v-if="accounts.error.value">
        {{ accounts.error.value }}
      </UiAlert>

      <form v-if="!flow" class="grid gap-5" @submit.prevent="add">
        <div class="grid grid-cols-2 gap-2.5 phone:grid-cols-1" role="radiogroup" aria-label="Coding agent">
          <button
            v-for="(item, key) in providers"
            :key="key"
            type="button"
            role="radio"
            :aria-checked="chosen === key"
            class="flex items-center gap-3 rounded-2xl border px-3 py-3 text-left transition-[border-color,background,box-shadow] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
            :class="chosen === key ? brands[key].chosen : 'border-line hover:bg-hover/40'"
            @click="chosen = key"
          >
            <ProviderMark :provider="key" :size="32" />
            <span class="min-w-0">
              <span class="block text-sm leading-tight font-semibold">{{ item.label }}</span>
              <span class="text-2xs text-muted">{{ item.subscription }}</span>
            </span>
          </button>
        </div>
        <label class="gap-1.5 text-sm text-ink">
          Name
          <input
            v-model="name"
            maxlength="100"
            autocomplete="off"
            placeholder="e.g. Work"
          >
        </label>
        <p class="text-xs text-muted">
          You sign in on {{ providers[chosen].vendor }}’s page. Your password stays there.
        </p>
        <UiButton type="submit" variant="primary" :disabled="busy">
          Continue to sign-in<Icon :name="ArrowUpRight" :size="15" />
        </UiButton>
      </form>

      <template v-else-if="flow.state === 'failed'">
        <p role="alert" class="text-sm text-danger">
          {{ flow.error || 'Sign-in did not finish.' }}
        </p>
        <div class="flex flex-wrap gap-2">
          <UiButton variant="primary" :disabled="busy" @click="accounts.reconnect(flow.accountId)">
            Try again
          </UiButton>
          <UiButton :disabled="busy" @click="close">
            Close
          </UiButton>
        </div>
      </template>

      <template v-else>
        <ol class="grid gap-1" aria-live="polite">
          <template v-if="flow.provider === 'codex'">
            <li class="flex gap-3">
              <span class="step-number" :class="flow.code ? 'done' : 'current'"><Icon v-if="flow.code" :name="Check" :size="14" /><Icon
                v-else
                :name="LoaderCircle"
                :size="14"
                class="animate-spin motion-reduce:animate-none"
              /></span>
              <p class="pt-1 text-sm font-semibold">
                {{ flow.code ? 'Code ready' : 'Getting a secure code from Codex…' }}
              </p>
            </li>
            <li class="flex gap-3">
              <span class="step-number" :class="flow.code && flow.phase !== 'verifying' ? 'current' : flow.phase === 'verifying' ? 'done' : ''">2</span>
              <div class="min-w-0 flex-1 pt-1">
                <p class="text-sm font-semibold">
                  Enter this code on OpenAI
                </p>
                <template v-if="flow.code && flow.phase !== 'verifying'">
                  <div class="mt-2.5 flex items-center justify-between gap-3 rounded-2xl border border-line bg-inset px-4 py-3">
                    <code aria-label="Verification code" class="min-w-0 select-all font-mono text-2xl font-bold tracking-widest break-all phone:text-xl">{{ flow.code }}</code>
                    <button
                      type="button"
                      class="grid size-8 shrink-0 place-items-center rounded-lg text-muted hover:bg-hover hover:text-ink"
                      :aria-label="copied ? 'Code copied' : 'Copy verification code'"
                      @click="copyCode"
                    >
                      <Icon :name="copied ? Check : Copy" :size="16" />
                    </button>
                  </div>
                  <p v-if="copyError" role="alert" class="mt-2 text-sm text-warning">
                    {{ copyError }}
                  </p>
                  <a
                    v-if="flow.url"
                    :href="flow.url"
                    target="_blank"
                    rel="noopener noreferrer"
                    class="mt-3 w-full"
                    :class="[buttonBase, buttonVariants.primary, buttonSizes.default]"
                    @click="copyCode"
                  >Copy code &amp; open sign-in<Icon :name="ArrowUpRight" :size="15" /></a>
                </template>
              </div>
            </li>
          </template>
          <template v-else>
            <li class="flex gap-3">
              <span class="step-number" :class="flow.url ? 'done' : 'current'"><Icon v-if="flow.url" :name="Check" :size="14" /><Icon
                v-else
                :name="LoaderCircle"
                :size="14"
                class="animate-spin motion-reduce:animate-none"
              /></span>
              <div class="min-w-0 flex-1 pt-1">
                <p class="text-sm font-semibold">
                  {{ flow.url ? 'Sign in on Anthropic’s page' : 'Preparing your secure sign-in link…' }}
                </p>
                <a
                  v-if="flow.url && flow.phase !== 'verifying'"
                  :href="flow.url"
                  target="_blank"
                  rel="noopener noreferrer"
                  class="mt-2.5"
                  :class="[buttonBase, buttonVariants.default, buttonSizes.small]"
                >Open Claude sign-in<Icon :name="ArrowUpRight" :size="14" /></a>
              </div>
            </li>
            <li class="flex gap-3">
              <span class="step-number" :class="flow.acceptsCode ? 'current' : flow.phase === 'verifying' ? 'done' : ''">2</span>
              <form class="min-w-0 flex-1 pt-1" @submit.prevent="submit">
                <label class="grid gap-2 text-sm font-semibold">
                  Paste the code Anthropic shows
                  <input
                    v-model="code"
                    type="password"
                    autocomplete="off"
                    spellcheck="false"
                    placeholder="Authorization code"
                    :disabled="busy || !flow.acceptsCode || submitted"
                    aria-label="Claude authorization code"
                  >
                </label>
                <UiButton
                  v-if="flow.acceptsCode && !submitted"
                  type="submit"
                  variant="primary"
                  class="mt-3 w-full"
                  :disabled="busy || !code.trim()"
                >
                  Finish sign-in
                </UiButton>
              </form>
            </li>
          </template>
          <li class="flex gap-3">
            <span class="step-number" :class="flow.phase === 'verifying' || submitted ? 'current' : ''">3</span>
            <p class="flex items-center gap-2 pt-1 text-sm" :class="flow.phase === 'verifying' || submitted ? 'font-semibold' : 'text-muted'">
              <Icon
                v-if="flow.phase === 'verifying' || submitted"
                :name="LoaderCircle"
                :size="14"
                class="animate-spin motion-reduce:animate-none"
              />
              {{ flow.phase === 'verifying' || submitted ? 'Verifying your account…' : 'Approve access — this window finishes on its own' }}
            </p>
          </li>
        </ol>
        <UiButton size="small" :disabled="busy || flow.phase === 'verifying'" @click="close">
          Cancel sign-in
        </UiButton>
      </template>
    </section>
  </Modal>
</template>

<style scoped>
.step-number {
  display: grid;
  place-items: center;
  width: 26px;
  height: 26px;
  flex-shrink: 0;
  border-radius: 50%;
  font: 700 12px var(--font-heading);
  background: var(--color-hover);
  color: var(--color-muted);
}
.step-number.current {
  background: var(--color-accent);
  color: #fff;
}
.step-number.done {
  background: var(--color-soft);
  color: var(--color-accent);
}
</style>
