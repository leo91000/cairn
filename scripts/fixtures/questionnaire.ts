import type { ChatQuestion } from '../../packages/contracts/chats'
import type { RunEvent } from '../../packages/contracts/contracts'
import { createApp, h, shallowRef } from 'vue'
import ActivityFeed from '../../apps/web/src/components/ActivityFeed.vue'
import ChatQuestions from '../../apps/web/src/components/ChatQuestions.vue'
import '../../apps/web/src/styles/index.css'

const questions = shallowRef<ChatQuestion[]>([])
const history = shallowRef<RunEvent[]>([])
window.questionnaireHistory = (text: string) => {
  history.value = [{
    id: 1,
    runId: 'run',
    createdAt: 1,
    type: 'chat.user',
    text,
    payload: { text, questionId: 'question' },
  }]
}

window.questionnaireRaw = (fields: unknown) => {
  questions.value = [{
    id: 'question',
    chatId: 'chat',
    runId: 'run',
    blocking: false,
    status: 'pending',
    createdAt: 1,
    fields: fields as ChatQuestion['fields'],
  }]
}

window.questionnaire = (fields: unknown) => {
  questions.value = [{
    id: 'question',
    chatId: 'chat',
    runId: 'run',
    blocking: false,
    status: 'pending',
    createdAt: 1,
    fields: fields as ChatQuestion['fields'],
  }]
}

createApp({
  render: () => h('main', [h(ChatQuestions, {
    questions: questions.value,
    active: false,
    onAnswered: () => {
      questions.value = []
    },
  }), h(ActivityFeed, {
    events: history.value,
    active: false,
    agent: 'Cairn',
    task: 'Questionnaire',
    more: false,
    loading: false,
    trimmed: 0,
    chat: true,
  })]),
}).mount('#app')
declare global {
  interface Window { questionnaireRaw: (fields: unknown) => void, questionnaireHistory: (text: string) => void, questionnaire: (fields: unknown) => void }
}
