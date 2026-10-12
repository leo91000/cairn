import type { FilItem } from './signal'
import {
  onBeforeUnmount,
  onMounted,
  reactive,
  watch,
} from 'vue'
import { api, notify } from './api'

/**
 * Fil items read here, by the end of the agent's work that was read, until the server's lists
 * catch up. Keeping the finish time rather than a clock reading lets a later turn show again.
 */
export const localReads = reactive<Record<string, number>>({})

/** Marks finished work as read for the signed-in account, on every device. */
export async function markRead(items: Pick<FilItem, 'key' | 'at'>[]) {
  const unread = items.filter(item => !(localReads[item.key] >= item.at))
  if (!unread.length)
    return
  const previous = unread.map(item => [item.key, localReads[item.key]] as const)
  for (const item of unread)
    localReads[item.key] = item.at
  try {
    await api('/reads', { method: 'POST', body: JSON.stringify({ items: unread.map(item => item.key) }) })
  }
  catch (error) {
    for (const [key, at] of previous) {
      if (at === undefined)
        delete localReads[key]
      else
        localReads[key] = at
    }

    notify((error as Error).message)
  }
}

/**
 * Reads what is open while the page is visible: a turn that finishes in front of the user is
 * never left unread, and one that finished in a background tab waits until the tab returns.
 */
export function useReadWhileOpen(item: () => Pick<FilItem, 'key' | 'at'> | null | undefined) {
  const read = () => {
    const current = item()
    if (current && !document.hidden)
      void markRead([current])
  }

  // After rendering, so the Fil beside sees the item unread first and keeps it in place, read.
  watch(() => {
    const current = item()
    return current ? `${current.key}@${current.at}` : ''
  }, read, { flush: 'post' })
  onMounted(() => {
    read()
    document.addEventListener('visibilitychange', read)
  })
  onBeforeUnmount(() => document.removeEventListener('visibilitychange', read))
}
