import type { ChatQuestion } from '../../packages/contracts/chats'
import { createApp, h, shallowRef } from 'vue'
import ChatQuestions from '../../apps/web/src/components/ChatQuestions.vue'
import { questionFields } from '../../packages/contracts/chats'
import '../../apps/web/src/styles/index.css'

const questions = shallowRef<ChatQuestion[]>([])
window.questionnaire = (fields: unknown) => {
  questions.value = [{
    id: 'question',
    chatId: 'chat',
    runId: 'run',
    blocking: false,
    status: 'pending',
    createdAt: 1,
    fields: questionFields.parse(fields),
  }]
}

createApp({
  render: () => h(ChatQuestions, {
    questions: questions.value,
    active: false,
    onAnswered: () => {
      questions.value = []
    },
  }),
}).mount('#app')
declare global {
  interface Window { questionnaire: (fields: unknown) => void }
}
