<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { api, officialEntry, redirect } from '../api'
import AccountConfirmation from '../components/AccountConfirmation.vue'
import Icon from '../components/Icon.vue'
import UiAlert from '../components/UiAlert.vue'
import UiButton from '../components/UiButton.vue'
import { ShieldCheck } from '../icons'

const parameters = Object.fromEntries(new URLSearchParams(location.search))
const details = ref<any>()
const redirectHost = computed(() => details.value?.client.redirect_uri ? new URL(details.value.client.redirect_uri).host : '')
const error = ref('')
const busy = ref(false)
const installationId = ref('')
const confirmingIdentity = ref(false)
const endpoint = officialEntry ? '/mcp/oauth' : '/oauth'
onMounted(async () => {
  try {
    details.value = await api(`${endpoint}/preview`, {
      method: 'POST',
      body: JSON.stringify(parameters),
    })
    installationId.value = details.value.installations?.[0]?.id || ''
  }
  catch (e) {
    error.value = (e as Error).message
  }
})

async function consent(approved: boolean) {
  busy.value = true
  try {
    const result = await api(`${endpoint}/consent`, {
      method: 'POST',
      body: JSON.stringify({ parameters, approved, installationId: installationId.value }),
    })
    redirect(result.redirect)
  }
  catch (e) {
    error.value = (e as Error).message
    busy.value = false
  }
}
</script>

<template>
  <section class="panel bg-surface border border-line rounded-card overflow-hidden consent-card max-w-147.5 p-10 mx-auto my-[35px] phone:p-[25px] phone:mx-auto phone:my-2.5">
    <AccountConfirmation
      v-if="confirmingIdentity"
      title="Confirm identity before granting access to this assistant"
      return-label="Back to consent"
      @close="confirmingIdentity = false"
      @confirmed="confirmingIdentity = false; error = ''"
    />
    <div v-show="!confirmingIdentity">
      <span class="empty-icon grid place-items-center w-16 h-16 rounded-[19px] bg-surface text-muted mb-5.5 border border-line"><Icon :name="ShieldCheck" :size="30" /></span>
      <h1>Connect an assistant</h1>
      <UiAlert v-if="error">
        {{ error }}
      </UiAlert>
      <template v-if="details">
        <p>
          <strong>{{ details.client.client_name }}</strong> is requesting access
          to one Leo installation.
        </p>
        <template v-if="officialEntry">
          <p>Redirect destination: <strong class="break-all">{{ redirectHost }}</strong></p>
          <UiAlert>
            Unverified client. Its name is supplied by the client and has not been checked by Leo.
            Continue only if you trust this redirect destination.
          </UiAlert>
        </template>
        <label v-if="officialEntry">Installation
          <select v-model="installationId" aria-label="Installation">
            <option v-for="item in details.installations" :key="item.id" :value="item.id">
              {{ item.name }} · {{ item.online ? 'Online' : 'Offline' }}
            </option>
          </select>
        </label>
        <p v-if="officialEntry && !details.installations.length">
          Add an installation before granting access.
        </p>
        <ul class="consent-permissions pr-5 pl-[33px] leading-[2] bg-surface rounded-[9px] text-sm text-muted py-5 mx-0 my-6">
          <li v-for="scope in details.scopes" :key="scope">
            {{
              scope === "read"
                ? "Read tasks, profiles, skills, and run results"
                : scope === "run"
                  ? "Start and cancel tasks in YOLO mode with full access inside a private VM"
                  : "Create and modify tasks, projects, agents, and skills"
            }}
          </li>
        </ul>
        <p class="muted text-muted">
          You can revoke this connection at any time in Settings.
        </p>
        <div class="consent-actions flex flex-wrap justify-end gap-3 mt-[25px]">
          <UiButton v-if="officialEntry" :disabled="busy" @click="confirmingIdentity = true">
            Confirm identity
          </UiButton>
          <UiButton :disabled="busy" @click="consent(false)">
            Deny
          </UiButton><UiButton variant="primary" :disabled="busy || (officialEntry && !installationId)" @click="consent(true)">
            Allow access
          </UiButton>
        </div>
      </template>
    </div>
  </section>
</template>
