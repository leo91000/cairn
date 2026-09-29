<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { api, redirect } from '../api'
import Icon from '../components/Icon.vue'
import UiAlert from '../components/UiAlert.vue'
import UiButton from '../components/UiButton.vue'
import { ShieldCheck } from '../icons'

const parameters = Object.fromEntries(new URLSearchParams(location.search))
const details = ref<any>()
const error = ref('')
const busy = ref(false)
onMounted(async () => {
  try {
    details.value = await api('/oauth/preview', {
      method: 'POST',
      body: JSON.stringify(parameters),
    })
  }
  catch (e) {
    error.value = (e as Error).message
  }
})
async function consent(approved: boolean) {
  busy.value = true
  try {
    const result = await api('/oauth/consent', {
      method: 'POST',
      body: JSON.stringify({ parameters, approved }),
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
    <span class="empty-icon grid place-items-center w-16 h-16 rounded-[19px] bg-surface text-muted mb-5.5 border border-line"><Icon :name="ShieldCheck" :size="30" /></span>
    <h1>Connect an assistant</h1>
    <UiAlert v-if="error">
      {{ error }}
    </UiAlert>
    <template v-if="details">
      <p>
        <strong>{{ details.client.client_name }}</strong> is requesting access
        to your Leo workspace.
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
      <div class="consent-actions flex justify-end gap-3 mt-[25px]">
        <UiButton :disabled="busy" @click="consent(false)">
          Deny
        </UiButton><UiButton variant="primary" :disabled="busy" @click="consent(true)">
          Allow access
        </UiButton>
      </div>
    </template>
  </section>
</template>
