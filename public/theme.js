// Runs before the app and its styles so a saved preference never flashes light.
(() => {
  const key = 'leo.theme'
  const root = document.documentElement
  const media = window.matchMedia('(prefers-color-scheme: dark)')
  const subscribers = new Set()
  const normalize = value => ['light', 'dark', 'system'].includes(value) ? value : 'system'
  let preference = 'system'
  try {
    preference = normalize(localStorage.getItem(key))
  }
  catch { /* Privacy settings may disable storage; system mode still works. */ }

  function apply() {
    const resolved = preference === 'system' ? media.matches ? 'dark' : 'light' : preference
    root.dataset.theme = resolved
    root.dataset.themePreference = preference
    root.style.colorScheme = resolved
    document.querySelector('meta[name="theme-color"]')?.setAttribute('content', resolved === 'dark' ? '#0f0f14' : '#f4f2ec')
    for (const subscriber of subscribers)
      subscriber(preference)
  }

  window.leoTheme = {
    getPreference: () => preference,
    setPreference(value) {
      preference = normalize(value)
      try {
        localStorage.setItem(key, preference)
      }
      catch { /* Keep the selection for this page when storage is unavailable. */ }

      apply()
    },
    subscribe(callback) {
      subscribers.add(callback)
      return () => subscribers.delete(callback)
    },
  }
  media.addEventListener('change', apply)
  window.addEventListener('storage', (event) => {
    if (event.key !== key && event.key !== null)
      return
    preference = normalize(event.newValue)
    apply()
  })
  apply()
})()
