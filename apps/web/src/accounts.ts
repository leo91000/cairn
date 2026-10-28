import type {
  Account,
  Accounts,
  AccountStatus,
  Provider,
  SignIn,
  UsageWindow,
} from '../../../packages/contracts/accounts'
import type { IconName } from './icons'
import { onBeforeUnmount, onMounted, ref } from 'vue'
import { cancelCodingAgentAccountSignIn } from '../../../packages/contracts/generated/cancelCodingAgentAccountSignIn'
import { connectCodingAgentAccount } from '../../../packages/contracts/generated/connectCodingAgentAccount'
import { deleteCodingAgentAccount } from '../../../packages/contracts/generated/deleteCodingAgentAccount'
import { listCodingAgentAccounts } from '../../../packages/contracts/generated/listCodingAgentAccounts'
import { orderCodingAgentAccounts } from '../../../packages/contracts/generated/orderCodingAgentAccounts'
import { reconnectCodingAgentAccount } from '../../../packages/contracts/generated/reconnectCodingAgentAccount'
import { refreshCodingAgentAccounts } from '../../../packages/contracts/generated/refreshCodingAgentAccounts'
import { replaceCodingAgentAccount } from '../../../packages/contracts/generated/replaceCodingAgentAccount'
import { replaceCodingAgentAccountKey } from '../../../packages/contracts/generated/replaceCodingAgentAccountKey'
import { submitCodingAgentAccountCode } from '../../../packages/contracts/generated/submitCodingAgentAccountCode'
import { api, notify } from './api'
import { BrandClaude, BrandOpenAI, BrandOpenCode } from './icons'

/** Each coding agent's brand mark and colors. */
export const brands: Record<Provider, {
  icon: IconName
  text: string
  tile: string
  chosen: string
}> = {
  codex: {
    icon: BrandOpenAI,
    text: 'text-ink',
    tile: 'bg-ink text-canvas',
    chosen: 'border-accent bg-accent/8 shadow-[0_0_0_1px_var(--color-accent)]',
  },
  claude: {
    icon: BrandClaude,
    text: 'text-claude',
    tile: 'bg-claude text-white',
    chosen: 'border-claude bg-claude/8 shadow-[0_0_0_1px_var(--color-claude)]',
  },
  opencode: {
    icon: BrandOpenCode,
    text: 'text-ink',
    tile: 'bg-ink text-canvas',
    chosen: 'border-accent bg-accent/8 shadow-[0_0_0_1px_var(--color-accent)]',
  },
}

/** Short status shown beside an account. Ready accounts need no label. */
export const statusLabels: Record<AccountStatus, string> = {
  signIn: 'Finish sign-in',
  reconnect: 'Reconnect',
  paused: 'Paused',
  unavailable: 'Usage unavailable',
  waiting: 'Waiting for reset',
  full: 'All slots busy',
  next: 'Next up',
  low: 'Low',
  ready: '',
}
export const statusTones: Record<AccountStatus, 'accent' | 'warning' | 'muted' | 'attention'> = {
  signIn: 'attention',
  reconnect: 'attention',
  paused: 'muted',
  unavailable: 'muted',
  waiting: 'muted',
  full: 'muted',
  next: 'accent',
  low: 'warning',
  ready: 'muted',
}

/** "5 h" and "Week" beside the bars; model windows keep their label. */
export function windowName(window: UsageWindow) {
  if (window.models.length)
    return window.label
  if (window.durationMins === 10080)
    return 'Week'
  if (window.durationMins && window.durationMins % 60 === 0 && window.durationMins < 1440)
    return `${window.durationMins / 60} h`
  return window.label
}

/** "in 2h 10m" within a day, then the local weekday and time. */
export function resetsIn(seconds: number | null | undefined, now = Date.now()) {
  if (!seconds)
    return ''
  const minutes = Math.ceil((seconds * 1000 - now) / 60000)
  if (minutes <= 0)
    return 'now'
  if (minutes < 60)
    return `in ${minutes}m`
  if (minutes < 1440)
    return `in ${Math.floor(minutes / 60)}h ${String(minutes % 60).padStart(2, '0')}m`
  return new Intl.DateTimeFormat(undefined, { weekday: 'short', hour: '2-digit', minute: '2-digit' }).format(seconds * 1000)
}

/** Accounts of every coding agent and the sign-in in progress, kept current while mounted. */
export function useAccounts() {
  const accounts = ref<Account[]>([])
  const signIn = ref<SignIn | null>(null)
  const required = ref<Provider[]>([])
  const loaded = ref(false)
  const busy = ref(false)
  const error = ref('')
  let timer: ReturnType<typeof setTimeout> | undefined
  let disposed = false

  function apply(value: Accounts) {
    const previous = signIn.value
    accounts.value = value.accounts
    signIn.value = value.signIn
    required.value = value.required ?? []
    if (previous?.state === 'pending' && value.signIn?.state === 'complete') {
      const account = value.accounts.find(a => a.id === value.signIn!.accountId)
      notify(`${account?.name ?? 'Account'} connected`)
    }
  }

  async function load() {
    try {
      apply(await listCodingAgentAccounts(api, {}))
      error.value = ''
    }
    catch (e) {
      error.value = (e as Error).message
    }
    finally {
      loaded.value = true
    }
  }

  async function poll() {
    clearTimeout(timer)
    await load()
    if (!disposed)
      timer = setTimeout(poll, signIn.value?.state === 'pending' ? 1500 : 5000)
  }

  async function action<T>(operation: () => Promise<T>): Promise<T | undefined> {
    busy.value = true
    error.value = ''
    try {
      const result = await operation()
      await poll()
      return result
    }
    catch (e) {
      error.value = (e as Error).message
      return undefined
    }
    finally {
      busy.value = false
    }
  }

  const refreshOnFocus = () => void poll()
  onMounted(() => {
    void poll()
    window.addEventListener('focus', refreshOnFocus)
  })
  onBeforeUnmount(() => {
    disposed = true
    clearTimeout(timer)
    window.removeEventListener('focus', refreshOnFocus)
  })
  return {
    accounts,
    signIn,
    required,
    loaded,
    busy,
    error,
    refresh: () => action(async () => apply(await refreshCodingAgentAccounts(api, {}))),
    add: (provider: Provider, name: string) => action(async () => {
      const result = await connectCodingAgentAccount(api, {
        provider,
        name,
        modelProvider: null,
        apiKey: null,
      })
      if (result.kind === 'signIn')
        return result.signIn
      throw new Error('Connect this account in Connections with an owner session.')
    }),
    connectKey: (provider: Provider, name: string, modelProvider: string, apiKey: string, id?: string) => action(async () => {
      const result = id
        ? await replaceCodingAgentAccountKey(api, { accountId: id, modelProvider, apiKey })
        : await connectCodingAgentAccount(api, {
            provider,
            name,
            modelProvider,
            apiKey,
          })
      if (result.kind === 'connected')
        return result.account
      throw new Error('Connect this account in Connections with an owner session.')
    }),
    reconnect: (id: string) => action(async () => {
      const result = await reconnectCodingAgentAccount(api, { accountId: id })
      if (result.kind === 'signIn')
        return result.signIn
      throw new Error('Reconnect this account in Connections with an owner session.')
    }),
    submitCode: (code: string) => action(async () => {
      await submitCodingAgentAccountCode(api, { code })
      return true
    }),
    cancel: () => action(() => cancelCodingAgentAccountSignIn(api, {})),
    update: (id: string, patch: Partial<Pick<Account, 'name' | 'enabled' | 'maxConcurrentRuns'>>) => action(() => {
      const account = accounts.value.find(account => account.id === id)
      if (!account)
        throw new Error('Reload the account and try again.')
      return replaceCodingAgentAccount(api, {
        accountId: id,
        revision: account.revision,
        settings: {
          name: account.name,
          enabled: account.enabled,
          maxConcurrentRuns: account.maxConcurrentRuns,
          ...patch,
        },
      })
    }),
    reorder: (provider: Provider, ids: string[]) => action(async () => apply(await orderCodingAgentAccounts(api, { provider, ids }))),
    remove: (id: string) => action(async () => {
      await deleteCodingAgentAccount(api, { accountId: id })
      return true
    }),
  }
}

export type AccountsState = ReturnType<typeof useAccounts>
