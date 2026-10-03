<script setup lang="ts">
import type { ChatDetail, ChatView } from '../shared/chats'
import { onMounted, ref } from 'vue'
import { api } from './api'
import Markdown from './components/Markdown.vue'
import UiAlert from './components/UiAlert.vue'
import UiButton from './components/UiButton.vue'

const chats = ref<ChatView[]>([])
const detail = ref<ChatDetail | null>(null)
const message = ref('')
const busy = ref(false)
const error = ref('')

async function perform(action: () => Promise<void>) {
  busy.value = true
  error.value = ''
  try {
    await action()
  }
  catch (cause) {
    error.value = cause instanceof Error ? cause.message : 'Unable to reach your installation.'
  }
  finally {
    busy.value = false
  }
}

async function list() {
  await perform(async () => {
    chats.value = await api<ChatView[]>('/chats')
    detail.value = null
  })
}

async function open(id: string) {
  await perform(async () => {
    detail.value = await api<ChatDetail>(`/chats/${encodeURIComponent(id)}`)
    message.value = ''
  })
}

async function create() {
  await perform(async () => {
    const chat = await api<ChatView>('/chats', { method: 'POST', body: '{}' })
    detail.value = await api<ChatDetail>(`/chats/${encodeURIComponent(chat.id)}`)
    message.value = ''
  })
}

async function send() {
  if (!detail.value || !message.value.trim())
    return
  const path = `/chats/${encodeURIComponent(detail.value.id)}`
  await perform(async () => {
    await api(`${path}/messages`, {
      method: 'POST',
      body: JSON.stringify({ id: crypto.randomUUID(), text: message.value }),
    })
    // Clear after acceptance, even if the following read fails: never resend a write.
    message.value = ''
    detail.value = await api<ChatDetail>(path)
  })
}

onMounted(list)
</script>

<template>
  <div class="grid gap-4 mb-8" :aria-busy="busy">
    <UiAlert v-if="error">
      {{ error }}
    </UiAlert>
    <template v-if="detail">
      <UiButton :disabled="busy" @click="list">
        Back to conversations
      </UiButton>
      <h2 class="font-heading text-xl break-words">
        {{ detail.title }}
      </h2>
      <UiButton :disabled="busy" @click="open(detail.id)">
        Refresh conversation
      </UiButton>
      <ul aria-label="Pending messages" class="grid gap-3">
        <li v-for="item in detail.messages" :key="item.id" class="whitespace-pre-wrap break-words">
          {{ item.text }}
        </li>
      </ul>
      <UiAlert v-if="detail.run?.error">
        {{ detail.run.error }}
      </UiAlert>
      <section v-else-if="detail.run?.summary" aria-label="Agent response" class="min-w-0 break-words">
        <Markdown :content="detail.run.summary" />
      </section>
      <form class="grid gap-3" @submit.prevent="send">
        <label>Message<textarea
          v-model="message"
          required
          maxlength="50000"
          :disabled="busy"
        /></label>
        <UiButton type="submit" :disabled="busy || !message.trim()" variant="primary">
          Send message
        </UiButton>
      </form>
    </template>
    <template v-else>
      <h2 class="font-heading text-xl">
        Conversations
      </h2>
      <UiButton
        v-for="chat in chats"
        :key="chat.id"
        :disabled="busy"
        @click="open(chat.id)"
      >
        {{ chat.title }}
      </UiButton>
      <UiButton :disabled="busy" @click="create">
        New conversation
      </UiButton>
      <UiButton :disabled="busy" @click="list">
        Refresh conversations
      </UiButton>
    </template>
  </div>
</template>
