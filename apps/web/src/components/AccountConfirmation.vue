<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { accountApi, redirect } from '../api'
import AccountSecurity from './AccountSecurity.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'

const props = defineProps<{ title: string, returnLabel: string }>()
const emit = defineEmits<{ close: [], confirmed: [] }>()
const account = ref<{ email: string }>()
const passkeys = ref(false)
const error = ref('')

async function load() {
  error.value = ''
  try {
    const [session, options] = await Promise.all([accountApi('/session'), accountApi('/options')])
    if (!session.authenticated) {
      redirect('/')
      return
    }

    account.value = session.account
    passkeys.value = options.passkeys
  }
  catch (cause) {
    error.value = cause instanceof Error ? cause.message : 'Unable to load account confirmation.'
  }
}

onMounted(load)
</script>

<template>
  <AccountSecurity
    v-if="account"
    :email="account.email"
    :passkeys="passkeys"
    confirmation-only
    :confirmation-title="props.title"
    :return-label="props.returnLabel"
    @close="emit('close')"
    @confirmed="emit('confirmed')"
    @signed-out="redirect('/')"
  />
  <div v-else class="grid gap-3">
    <UiAlert v-if="error">
      {{ error }}
    </UiAlert>
    <p v-else role="status">
      Loading account confirmation…
    </p>
    <UiButton v-if="error" @click="load">
      Try again
    </UiButton>
    <UiButton @click="emit('close')">
      {{ props.returnLabel }}
    </UiButton>
  </div>
</template>
