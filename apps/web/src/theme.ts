import { ref } from 'vue'

export type ThemePreference = 'system' | 'light' | 'dark'
declare global {
  interface Window {
    cairnTheme: {
      getPreference: () => ThemePreference
      setPreference: (preference: ThemePreference) => void
      subscribe: (callback: (preference: ThemePreference) => void) => () => void
    }
  }
}
export const themePreference = ref<ThemePreference>(window.cairnTheme.getPreference())
window.cairnTheme.subscribe(value => themePreference.value = value)
export function setThemePreference(value: ThemePreference) {
  window.cairnTheme.setPreference(value)
}
