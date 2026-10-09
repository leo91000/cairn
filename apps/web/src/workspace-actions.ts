import type { InjectionKey } from 'vue'

export const workspaceActionsKey: InjectionKey<{ search: () => void, navigation: () => void }> = Symbol('workspace actions')
