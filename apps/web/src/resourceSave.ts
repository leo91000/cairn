import { ref } from 'vue'
import { api, ApiError } from './api'

// Each editor session owns one creation identity. An uncertain response is replayed
// with its original payload before applying any edits made since that attempt.
export function useResourceSave(initialId: string | null = null) {
  const savedId = ref(initialId)
  const pendingCreation = ref(false)
  let creationId = crypto.randomUUID()
  let attemptedCreation = false
  let uncertainCreation = false
  let pending: { path: string, body: string } | undefined

  function reset(id: string | null = null) {
    savedId.value = id
    creationId = crypto.randomUUID()
    attemptedCreation = false
    uncertainCreation = false
    pending = undefined
    pendingCreation.value = false
  }

  async function save<T extends { id: string }>(path: string, options: {
    body: () => object
    createPath?: string
    createBody?: (creationId: string) => object
    onCreated?: (saved: T) => void
  }): Promise<T> {
    if (savedId.value) {
      return api<T>(`${path}/${savedId.value}`, {
        method: 'PUT',
        body: JSON.stringify(options.body()),
      })
    }

    // A rejected retry may clear its payload after an earlier attempt committed.
    // Any later acquired ID must receive the latest editable form.
    const replaying = attemptedCreation
    attemptedCreation = true
    pending ??= {
      path: options.createPath ?? path,
      body: JSON.stringify(options.createBody?.(creationId) ?? { ...options.body(), creationId }),
    }
    pendingCreation.value = true

    let saved: T
    try {
      saved = await api<T>(pending.path, { method: 'POST', body: pending.body })
      if (typeof saved?.id !== 'string' || !saved.id)
        throw new Error('The save response did not include the resource identity.')
    }
    catch (error) {
      // Business validation is a known rejection; let the user correct the form.
      // Network, malformed response and server errors may follow a committed POST.
      if (error instanceof ApiError && error.status >= 400 && error.status < 500 && error.status !== 408) {
        pending = undefined
        pendingCreation.value = uncertainCreation
      }
      else {
        uncertainCreation = true
      }

      throw error
    }

    savedId.value = saved.id
    pending = undefined
    pendingCreation.value = false
    uncertainCreation = false
    options.onCreated?.(saved)

    if (replaying) {
      return api<T>(`${path}/${saved.id}`, {
        method: 'PUT',
        body: JSON.stringify(options.body()),
      })
    }

    return saved
  }

  return {
    savedId,
    pendingCreation,
    reset,
    save,
  }
}
