import type { ReadMark } from '../../../packages/contracts/generated/typescript/ReadMark'
import type { FilItem } from './signal'
import {
  onBeforeUnmount,
  onMounted,
  reactive,
  watch,
} from 'vue'
import { markRead as markReadContract } from '../../../packages/contracts/generated/markRead'
import { api, notify, state } from './api'

/**
 * Fil items read here, by the end of the agent's work that was read, until the server's lists
 * catch up. Keeping the finish time rather than a clock reading lets a later turn show again.
 */
export const localReads = reactive<Record<string, number>>({})
let generation = 0
watch(() => [state.accountId, state.installationId, state.authenticated, state.signingOut], () => {
  generation++
  for (const key of Object.keys(localReads))
    delete localReads[key]
}, { flush: 'sync' })

/** A Fil key (`chat:<id>` or `task:<id>`) and the finish of the work read. */
function mark(item: Pick<FilItem, 'key' | 'at'>): ReadMark {
  const [kind, id] = [item.key.slice(0, item.key.indexOf(':')), item.key.slice(item.key.indexOf(':') + 1)]
  const target: ReadMark['target'] = kind === 'chat' ? { kind: 'chat', chatId: id } : { kind: 'mission', taskId: id }
  return { target, finishedAt: item.at }
}

/** Marks finished work as read for the signed-in account, on every device. */
export async function markRead(items: Pick<FilItem, 'key' | 'at'>[]) {
  if (!state.authenticated || state.signingOut)
    return
  const unread = items.filter(item => !(localReads[item.key] >= item.at))
  if (!unread.length)
    return
  const current = generation
  for (let offset = 0; offset < unread.length; offset += 200) {
    if (current !== generation)
      return
    const batch = unread.slice(offset, offset + 200)
    const previous = batch.map(item => [item.key, localReads[item.key]] as const)
    for (const item of batch)
      localReads[item.key] = Math.max(localReads[item.key] ?? 0, item.at)
    try {
      await markReadContract(api, { items: batch.map(mark) })
    }
    catch (error) {
      if (current !== generation)
        return
      for (const [index, [key, at]] of previous.entries()) {
        if (localReads[key] !== batch[index].at)
          continue
        if (at === undefined)
          delete localReads[key]
        else
          localReads[key] = at
      }

      notify((error as Error).message)
      return
    }
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
