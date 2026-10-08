<script setup lang="ts">
import { onMounted, ref } from 'vue'
import AccountSecurity from './AccountSecurity.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'

interface SignInMethod {
  id: string
  kind: 'email' | 'google' | 'github' | 'passkey'
  label: string
}

const props = defineProps<{
  email: string
  options: { google: boolean, github: boolean, passkeys: boolean }
  request: (route: string, body?: unknown) => Promise<any>
  embedded?: boolean
}>()
const emit = defineEmits<{
  close: []
  oauth: [provider: 'google' | 'github']
  sessionUpdated: [session: any]
  signedOut: []
}>()
const accountRequest = props.request
const methods = ref<SignInMethod[]>([])
const passkeyName = ref('My passkey')
const challenge = ref('')
const code = ref('')
const busy = ref(false)
const error = ref('')
const confirmingIdentity = ref(false)
const methodNames = {
  email: 'Email',
  google: 'Google',
  github: 'GitHub',
  passkey: 'Passkey',
}
const oauth = (provider: 'google' | 'github') => emit('oauth', provider)

onMounted(async () => {
  busy.value = true
  try {
    methods.value = (await accountRequest('methods')).methods
  }
  catch (cause) {
    error.value = cause instanceof Error ? cause.message : 'Unable to load sign-in methods.'
  }
  finally {
    busy.value = false
  }
})

async function removeMethod(id: string) {
  busy.value = true
  error.value = ''
  try {
    await accountRequest('methods/remove', { id })
    methods.value = (await accountRequest('methods')).methods
  }
  catch (cause) {
    error.value = cause instanceof Error ? cause.message : 'Unable to remove sign-in method.'
  }
  finally {
    busy.value = false
  }
}

async function registerPasskey() {
  busy.value = true
  error.value = ''
  try {
    if (!window.PublicKeyCredential?.parseCreationOptionsFromJSON || !PublicKeyCredential.parseRequestOptionsFromJSON)
      throw new Error('Passkeys are unavailable in this browser. Use another sign-in method.')
    const start = await accountRequest('passkeys/register/start', {})
    const credential = await navigator.credentials.create({ publicKey: PublicKeyCredential.parseCreationOptionsFromJSON(start.options.publicKey) })
    if (!(credential instanceof PublicKeyCredential))
      throw new Error('Passkey operation cancelled. Please try again.')
    await accountRequest('passkeys/register/finish', {
      challenge: start.challenge,
      credential: credential.toJSON(),
      label: passkeyName.value,
    })
    methods.value = (await accountRequest('methods')).methods
    passkeyName.value = 'My passkey'
  }
  catch (cause) {
    error.value = cause instanceof DOMException
      ? 'Passkey operation cancelled or unavailable. You can use another sign-in method.'
      : cause instanceof Error ? cause.message : 'Unable to use this passkey.'
  }
  finally {
    busy.value = false
  }
}

async function enableEmail() {
  busy.value = true
  error.value = ''
  try {
    if (challenge.value) {
      emit('sessionUpdated', await accountRequest('verify', { challenge: challenge.value, code: code.value }))
      challenge.value = ''
      code.value = ''
      methods.value = (await accountRequest('methods')).methods
    }
    else {
      const start = await accountRequest('email-code', { email: props.email })
      challenge.value = start.challenge
    }
  }
  catch (cause) {
    error.value = cause instanceof Error ? cause.message : 'Unable to enable email sign-in.'
  }
  finally {
    busy.value = false
  }
}
</script>

<template>
  <section aria-label="Sign-in methods" :aria-busy="busy">
    <AccountSecurity
      v-if="confirmingIdentity"
      :email="email"
      :passkeys="options.passkeys"
      confirmation-only
      confirmation-title="Confirm identity before changing sign-in methods"
      return-label="Back to sign-in methods"
      @close="confirmingIdentity = false"
      @confirmed="confirmingIdentity = false; error = ''"
      @signed-out="emit('signedOut')"
    />
    <template v-else>
      <component :is="embedded ? 'h2' : 'h1'" class="font-heading text-2xl mb-4">
        Sign-in methods
      </component>
      <p class="text-muted mb-4">
        {{ email }} · Keep at least one sign-in method.
      </p>
      <UiAlert v-if="error">
        {{ error }}
      </UiAlert>
      <ul class="grid gap-4 mb-6">
        <li v-for="method in methods" :key="method.id" class="border border-line rounded-xl p-4 grid gap-2 min-w-0">
          <span>{{ methodNames[method.kind] }}</span>
          <span class="text-muted break-all">{{ method.label }}</span>
          <UiButton
            :disabled="busy || methods.length === 1"
            :aria-label="`Remove ${method.kind === 'passkey' ? method.label : `${methodNames[method.kind]} ${method.label}`}`"
            @click="removeMethod(method.id)"
          >
            Remove
          </UiButton>
        </li>
      </ul>
      <p class="text-muted mb-3">
        Adding or removing a sign-in method requires an email code or existing passkey confirmed in the last five minutes.
      </p>
      <UiButton
        class="mb-3"
        :disabled="busy"
        @click="confirmingIdentity = true"
      >
        Confirm identity
      </UiButton>
      <form v-if="options.passkeys" class="grid gap-3 mb-6" @submit.prevent="registerPasskey()">
        <label>Passkey name<input
          v-model="passkeyName"
          required
          maxlength="80"
          :disabled="busy"
        ></label>
        <UiButton type="submit" :disabled="busy">
          Add passkey
        </UiButton>
      </form>
      <div class="grid gap-3 mb-6">
        <UiButton v-if="options.google && !methods.some(method => method.kind === 'google')" :disabled="busy" @click="oauth('google')">
          Add Google
        </UiButton>
        <UiButton v-if="options.github && !methods.some(method => method.kind === 'github')" :disabled="busy" @click="oauth('github')">
          Add GitHub
        </UiButton>
        <form v-if="!methods.some(method => method.kind === 'email')" class="grid gap-3" @submit.prevent="enableEmail">
          <label v-if="challenge">Email code<input
            v-model="code"
            autocomplete="one-time-code"
            inputmode="numeric"
            required
            maxlength="8"
            :disabled="busy"
          ></label>
          <UiButton type="submit" :disabled="busy">
            {{ challenge ? 'Confirm email code' : 'Enable email sign-in' }}
          </UiButton>
        </form>
      </div>
      <UiButton v-if="!embedded" :disabled="busy" @click="emit('close')">
        Back to installations
      </UiButton>
    </template>
  </section>
</template>
