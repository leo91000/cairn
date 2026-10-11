import type { ComputedRef, InjectionKey } from 'vue'
import type { SelectOption } from './select'

// The beacon shell owns the account session; phone headers inside the workspace read the
// current installation from here since the global installation bar is hidden on phones.
export interface CurrentInstallation {
  id: string
  name: string
  role: 'owner' | 'member'
  online: boolean
  updateRequired: boolean
  status: string
  options: SelectOption[]
  email: string
  disabled: boolean
}

export interface InstallationControls {
  current: ComputedRef<CurrentInstallation | null>
  select: (id: string) => void
  add: () => void
  accountSettings: () => void
  signOut: () => void
}

export const installationKey: InjectionKey<InstallationControls> = Symbol('current installation')

export function routeLabel(route: 'direct' | 'relay') {
  return route === 'direct' ? 'Direct' : 'Relais'
}

// Green when online in direct, amber through the relay (or while an update is required), grey offline.
export function connectionTone(installation: Pick<CurrentInstallation, 'online' | 'updateRequired'> | null | undefined, route: 'direct' | 'relay') {
  if (installation?.updateRequired)
    return 'bg-warning'
  if (!installation?.online)
    return 'bg-muted'
  return route === 'direct' ? 'bg-success' : 'bg-warning'
}
