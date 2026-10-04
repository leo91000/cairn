<script setup lang="ts">
import {
  computed,
  onMounted,
  ref,
  watch,
} from 'vue'
import { api, notify } from '../api'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'

interface StorageSettings {
  configured: boolean
  integrated: boolean
  environmentManaged: boolean
  bucket: string
  endpoint: string
  region: string
}

const storage = ref<StorageSettings>()
const editing = ref(false)
const busy = ref(false)
const error = ref('')
const form = ref({
  bucket: '',
  endpoint: '',
  region: '',
  accessKeyId: '',
  secretAccessKey: '',
  privateBucketConfirmed: false,
})

const r2 = computed(() => {
  try {
    const endpoint = new URL(form.value.endpoint)
    return endpoint.protocol === 'https:' && endpoint.hostname.endsWith('.r2.cloudflarestorage.com')
  }
  catch {
    return false
  }
})

watch(() => [form.value.endpoint, form.value.bucket], () => {
  form.value.privateBucketConfirmed = false
})

onMounted(async () => {
  try {
    storage.value = await api('/settings/storage')
  }
  catch (cause) {
    error.value = (cause as Error).message
  }
})

async function save() {
  busy.value = true
  error.value = ''
  try {
    storage.value = await api('/settings/storage', { method: 'PUT', body: JSON.stringify(form.value) })
    form.value.accessKeyId = ''
    form.value.secretAccessKey = ''
    form.value.privateBucketConfirmed = false
    editing.value = false
    notify('Storage settings saved')
  }
  catch (cause) {
    error.value = (cause as Error).message
  }
  finally {
    busy.value = false
  }
}

async function check() {
  busy.value = true
  error.value = ''
  try {
    await api('/settings/storage/check', { method: 'POST', body: '{}' })
    notify('Storage write, read and deletion succeeded')
  }
  catch (cause) {
    error.value = (cause as Error).message
  }
  finally {
    busy.value = false
  }
}
</script>

<template>
  <section class="panel border-b border-line settings-section mb-5.5 px-0 py-7 phone:py-5">
    <h2>Conversation storage</h2>
    <UiAlert v-if="error" class="my-3">
      {{ error }}
    </UiAlert>
    <template v-if="storage">
      <p v-if="storage.integrated" class="text-sm text-muted my-3">
        Your disks use the integrated S3 storage on this machine. An external S3 protects new disks against losing this machine.
      </p>
      <p v-else-if="storage.configured" class="text-sm text-muted my-3 break-all">
        S3 bucket: {{ storage.bucket }} · {{ storage.endpoint }}
      </p>
      <p v-else class="text-sm text-muted my-3">
        Configure S3 before starting a conversation.
      </p>
      <p class="text-sm text-muted my-3">
        Changing the default applies to new disks. Existing disks keep their original storage; keep that storage available.
      </p>
      <p v-if="storage.environmentManaged" class="text-sm text-muted my-3">
        Storage is configured through server environment variables. Remove those overrides to use these settings.
      </p>
      <div class="flex gap-3 flex-wrap my-3">
        <UiButton :disabled="busy || storage.environmentManaged" @click="editing = !editing">
          Configure external S3
        </UiButton>
        <UiButton v-if="storage.configured" :disabled="busy" @click="check">
          Check storage
        </UiButton>
      </div>
      <form v-if="editing" class="grid gap-4 max-w-xl mt-5" @submit.prevent="save">
        <label>S3 endpoint<input
          v-model="form.endpoint"
          type="url"
          placeholder="https://s3.example.com"
          required
        ></label>
        <label>S3 bucket<input v-model="form.bucket" required maxlength="63"></label>
        <label>S3 region<input v-model="form.region" required maxlength="100"></label>
        <label>S3 access key<input
          v-model="form.accessKeyId"
          type="password"
          autocomplete="new-password"
          required
          maxlength="256"
        ></label>
        <label>S3 secret key<input
          v-model="form.secretAccessKey"
          type="password"
          autocomplete="new-password"
          required
          maxlength="256"
        ></label>
        <label v-if="r2" class="checkbox flex items-start gap-3">
          <input v-model="form.privateBucketConfirmed" type="checkbox" required>
          I confirm R2 public domains (including r2.dev) and bucket locks are disabled in Cloudflare. Leo cannot check these through S3.
        </label>
        <p class="text-sm text-muted">
          Use a dedicated private bucket without lifecycle rules or Object Lock. Leo checks access before saving.
        </p>
        <UiButton type="submit" :disabled="busy">
          {{ busy ? 'Checking storage…' : 'Save S3 storage' }}
        </UiButton>
      </form>
    </template>
  </section>
</template>
