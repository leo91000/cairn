import { ref } from 'vue'
import { installationSubdomains, rememberPreference, sharedPreference } from './beacon-location'

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
if (installationSubdomains && !sharedPreference('cairn_theme'))
  rememberPreference('cairn_theme', window.cairnTheme.getPreference())

export const themePreference = ref<ThemePreference>(window.cairnTheme.getPreference())
window.cairnTheme.subscribe(value => themePreference.value = value)
export function setThemePreference(value: ThemePreference) {
  window.cairnTheme.setPreference(value)
  rememberPreference('cairn_theme', value)
}
