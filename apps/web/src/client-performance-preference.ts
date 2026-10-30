interface PreferenceEnvironment {
  read: () => boolean | undefined
  write: (value: boolean) => void
}

/** An explicit choice survives blocked, missing or stale browser persistence. */
export function performancePreference(environment: PreferenceEnvironment) {
  let current = true
  let explicit: boolean | undefined

  function refresh() {
    let stored: boolean | undefined
    try {
      stored = environment.read()
    }
    catch { /* Keep the current tab's choice when browser storage is unavailable. */ }

    if (stored === explicit)
      explicit = undefined
    current = explicit ?? stored ?? current
    return current
  }

  refresh()
  return {
    refresh,
    set(value: boolean) {
      explicit = value
      current = value
      try {
        environment.write(value)
      }
      catch { /* Refresh distinguishes a persisted choice from a failed write. */ }

      return refresh()
    },
  }
}
