import type { ChatQuestion } from '../../packages/contracts/chats'
import type { TimelineEntry } from '../../packages/contracts/live'
import Ajv from 'ajv'
import addFormats from 'ajv-formats'
import { createApp, h, shallowRef } from 'vue'
import ActivityFeed from '../../apps/web/src/components/ActivityFeed.vue'
import ChatQuestions from '../../apps/web/src/components/ChatQuestions.vue'
import questionSchema from '../../packages/contracts/generated/ChatQuestion.json'
import '../../apps/web/src/styles/index.css'

const ajv = new Ajv({ strict: false, useDefaults: true })
addFormats(ajv)
const validate = ajv.compile<ChatQuestion>(questionSchema)
const questions = shallowRef<ChatQuestion[]>([])
const history = shallowRef<TimelineEntry[]>([])
window.questionnaireHistory = (text: string) => {
  // The server's card for the person's answer, as the conversation receives it.
  history.value = [{
    id: 'user:1',
    runId: 'run',
    position: 1,
    revision: 1,
    source: 1,
    turn: 0,
    attempt: 0,
    createdAt: 1,
    updatedAt: 1,
    content: {
      kind: 'userMessage',
      text,
      questionId: 'question',
      sentAt: 1,
      attachments: [],
    },
  }]
}

window.questionnaireRaw = (fields: unknown) => {
  questions.value = [{
    id: 'question',
    chatId: 'chat',
    runId: 'run',
    blocking: false,
    status: 'pending',
    source: 'cairn',
    createdAt: 1,
    fields: fields as ChatQuestion['fields'],
  }]
}

window.questionnaire = (fields: unknown) => {
  const value = {
    id: 'question',
    chatId: 'chat',
    runId: 'run',
    blocking: false,
    status: 'pending',
    source: 'cairn',
    createdAt: 1,
    fields: structuredClone(fields),
  }
  if (!validate(value))
    throw new Error(JSON.stringify(validate.errors))
  questions.value = [value]
}

createApp({
  render: () => h('main', [h(ChatQuestions, {
    questions: questions.value,
    active: false,
    onAnswered: () => {
      questions.value = []
    },
  }), h(ActivityFeed, {
    events: [],
    timeline: history.value,
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
