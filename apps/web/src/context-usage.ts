import type { ContextUsage } from '../../../packages/contracts/chats'

export interface ContextSummary {
  /** Short form for the header: `42K / 200K · 21%`, or `Unavailable`. */
  short: string
  /** The occupancy, or why it is unknown. */
  label: string
  /** Each known detail: cache, window, measurement. */
  details: string[]
  /** Share of the window in use, only when both values are reported. */
  percent: number | null
}

function tokens(value: number) {
  if (value >= 1_000_000)
    return `${Number((value / 1_000_000).toFixed(value % 1_000_000 ? 1 : 0))}M`
  if (value >= 1000)
    return `${Math.round(value / 1000)}K`
  return String(value)
}

/**
 * How full the agent's context is. A missing window keeps the share unknown, and a missing
 * report stays unavailable: nothing is inferred from cumulative usage.
 */
export function contextSummary(usage: ContextUsage | null | undefined): ContextSummary {
  if (!usage || !Number.isFinite(usage.contextTokens) || usage.contextTokens <= 0) {
    return {
      short: 'Unavailable',
      label: 'Context usage unavailable',
      details: [],
      percent: null,
    }
  }

  const window = usage.contextWindow && usage.contextWindow > 0 ? usage.contextWindow : null
  const percent = window ? Math.min(100, Math.round(usage.contextTokens / window * 100)) : null
  const estimate = usage.estimated ? '≈ ' : ''
  const used = `${estimate}${tokens(usage.contextTokens)}`
  const details = [
    usage.estimated ? 'Estimated by the coding agent' : 'Measured from the latest model request',
    window ? `Window ${usage.contextWindow!.toLocaleString('en-US')} tokens` : 'Window size not reported',
  ]
  if (typeof usage.cachedTokens === 'number' && usage.cachedTokens > 0)
    details.push(`${usage.cachedTokens.toLocaleString('en-US')} tokens read from cache`)
  return {
    short: window ? `${used} / ${tokens(window)} · ${percent}%` : `${used} tokens`,
    label: window
      ? `${estimate}${usage.contextTokens.toLocaleString('en-US')} of ${window.toLocaleString('en-US')} tokens (${percent}%)`
      : `${estimate}${usage.contextTokens.toLocaleString('en-US')} tokens · window unknown`,
    details,
    percent,
  }
}

export interface CompactionEntry {
  kind: 'compaction'
  id: string
  time: number
  /** `stopped`: its execution ended before the coding agent confirmed the compaction. */
  state: 'running' | 'done' | 'failed' | 'stopped'
  trigger: 'manual' | 'auto' | null
  preTokens: number | null
  postTokens: number | null
}

/**
 * How an event that ends a turn or run ends a compaction its coding agent never confirmed. A
 * coding agent may fail or be stopped without closing the compaction it started.
 */
export function compactionEnd(event: { type: string, text?: string }): 'failed' | 'stopped' | null {
  if (event.type === 'turn.failed' || event.type === 'runner.failed' || (event.type === 'status' && event.text === 'failed'))
    return 'failed'
  if (event.type === 'turn.started' || event.type === 'turn.completed' || (event.type === 'status' && ['succeeded', 'cancelled', 'interrupted'].includes(event.text ?? '')))
    return 'stopped'
  return null
}

/** The divider a context compaction shows as. The visible history above it is unchanged. */
export function compactionText(entry: CompactionEntry) {
  const titles = {
    running: 'Compacting the agent context…',
    failed: 'Context compaction failed',
    stopped: 'Context compaction stopped',
    done: entry.trigger === 'auto' ? 'Context compacted automatically' : 'Context compacted',
  }
  const title = titles[entry.state]
  const change = entry.preTokens && entry.postTokens
    ? `${tokens(entry.preTokens)} → ${tokens(entry.postTokens)} tokens`
    : ''
  return {
    title,
    detail: [change, entry.state === 'done' ? 'Earlier messages stay visible here' : ''].filter(Boolean).join(' · '),
  }
}
