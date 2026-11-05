import { shallowRef, triggerRef } from 'vue'

// One generation of client work per active document, including BFCache restores.
// Abort before notifying observers: closing a peer can synchronously cause fallback.
export const documentLifetime = shallowRef(new AbortController())

export function departDocument() {
  if (documentLifetime.value.signal.aborted)
    return
  documentLifetime.value.abort()
  triggerRef(documentLifetime)
}

export function resumeDocument() {
  if (documentLifetime.value.signal.aborted)
    documentLifetime.value = new AbortController()
}

export function documentSignal(signal?: AbortSignal | null) {
  const lifetime = documentLifetime.value.signal
  lifetime.throwIfAborted()
  return signal ? AbortSignal.any([lifetime, signal]) : lifetime
}

if (typeof window !== 'undefined') {
  window.addEventListener('pagehide', departDocument)
  window.addEventListener('pageshow', resumeDocument)
}
