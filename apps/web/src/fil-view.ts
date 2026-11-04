import { ref, watch } from 'vue'
import { state } from './api'

/** Keep the chosen reception view when opening a conversation beside the Fil. */
export const activeFilView = ref<'unread' | 'ongoing' | 'all'>('unread')
watch(() => [state.installationId, state.accountId], () => activeFilView.value = 'unread')
