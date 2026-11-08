import type { InjectionKey } from 'vue'

export const workspaceActionsKey: InjectionKey<{ search: () => void }> = Symbol('workspace actions')
