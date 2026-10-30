export const performanceScreens = ['STARTUP', 'FIL', 'CHAT', 'MISSIONS', 'RUN', 'ATELIER', 'SETTINGS', 'OTHER'] as const
export const performanceOperations = ['API_READ', 'API_WRITE', 'JSON_DECODE', 'CACHE_READ', 'CACHE_SAVE', 'HISTORY_MERGE', 'TIMELINE', 'MARKDOWN', 'LIVE_REDUCE', 'LIVE_CATCHUP'] as const
export type PerformanceScreen = typeof performanceScreens[number]
export type PerformanceOperation = typeof performanceOperations[number]
export type PerformanceOutcome = 'ok' | 'error' | 'cancelled'
export type ProbeReason = 'INTERACTION' | 'API' | 'NAVIGATION'
export type PerformanceConnection = 'NONE' | 'OFFLINE' | 'DIRECT' | 'RELAY'

export interface WebBuild {
  buildId: string
  buildType: 'debug' | 'release'
  browser: 'CHROME' | 'EDGE' | 'FIREFOX' | 'SAFARI' | 'OTHER'
}

export interface MeasurementScope {
  readonly epoch: number
  readonly screen: PerformanceScreen
}

interface Timing {
  operation: PerformanceOperation
  screen: PerformanceScreen
  count: number
  startedOnMainThread: number
  meanMs: number
  maxMs: number
  p95UpperBoundMs: number
}

interface Distribution {
  count: number
  total: number
  maximum: number
  buckets: number[]
}

interface Operation extends Distribution {
  operation: PerformanceOperation
  screen: PerformanceScreen
  errors: number
  cancelled: number
}

interface Probe extends Distribution {
  screen: PerformanceScreen
  reason: ProbeReason
  late: number
}

interface SlowSample {
  elapsedMs: number
  screen: PerformanceScreen
  kind: PerformanceOperation | 'EVENT_LOOP_DELAY' | 'LONG_TASK'
  durationMs: number
  outcome: PerformanceOutcome
}

const maximumDuration = 86_400_000
const maximumCount = 1_000_000_000_000
const buckets = [1, 4, 8, 16, 32, 64, 128, 256, 512, 1000, 2000, 5000, 10000, 30000, 60000, maximumDuration]
const screens = new Set<string>(performanceScreens)
const operations = new Set<string>(performanceOperations)
const browsers = new Set<string>(['CHROME', 'EDGE', 'FIREFOX', 'SAFARI', 'OTHER'])
const connections = new Set<string>(['NONE', 'OFFLINE', 'DIRECT', 'RELAY'])

function distribution(): Distribution {
  return {
    count: 0,
    total: 0,
    maximum: 0,
    buckets: buckets.map(() => 0),
  }
}

function add(stat: Distribution, duration: number) {
  if (stat.count >= maximumCount)
    return false
  const elapsed = Math.min(duration, maximumDuration)
  stat.count++
  stat.total += elapsed
  stat.maximum = Math.max(stat.maximum, elapsed)
  stat.buckets[buckets.findIndex(limit => elapsed <= limit)]!++
  return true
}

function percentile(stat: Distribution) {
  const target = Math.ceil(stat.count * 0.95)
  let count = 0
  for (const [index, value] of stat.buckets.entries()) {
    count += value
    if (count >= target)
      return Math.min(stat.maximum, buckets[index]!)
  }

  return stat.maximum
}

function windowId() {
  try {
    return globalThis.crypto.randomUUID()
  }
  catch { return null }
}

/** Fixed categories and bounded in-memory aggregates; never accepts paths or payloads. */
export class ClientPerformance {
  private id: string | null = null
  private epoch = 0
  private sequence = 0
  private started = 0
  private activeSince = 0
  private foreground = 0
  private active = false
  private currentScreen: PerformanceScreen = 'STARTUP'
  private screenHistory: Array<{ at: number, screen: PerformanceScreen }> = []
  private readonly timings = new Map<string, Operation>()
  private readonly probes = new Map<string, Probe>()
  private readonly longTasks = new Map<PerformanceScreen, Distribution>()
  private readonly slow: SlowSample[] = []
  private readonly pending = new Map<number, { operation: PerformanceOperation, scope: MeasurementScope, at: number }>()
  private nextPending = 0
  private omittedInFlight = 0
  private longTaskSupported = false
  private probeScheduler?: (reason: ProbeReason) => void

  constructor(
    private readonly clock = () => performance.now(),
    private readonly identify: () => string | null = windowId,
  ) {
    this.clear()
  }

  clear() {
    this.id = this.identify()
    this.epoch++
    this.sequence = 0
    this.started = this.clock()
    this.activeSince = this.started
    this.foreground = 0
    this.timings.clear()
    this.probes.clear()
    this.longTasks.clear()
    this.slow.length = 0
    this.pending.clear()
    this.omittedInFlight = 0
    this.screenHistory = [{ at: this.started, screen: this.currentScreen }]
  }

  setActive(active: boolean) {
    if (active === this.active)
      return
    const now = this.clock()
    if (this.active)
      this.foreground += Math.max(0, now - this.activeSince)
    this.active = active
    this.activeSince = now
    this.epoch++
    if (this.pending.size || this.omittedInFlight)
      this.sequence++
    this.pending.clear()
    this.omittedInFlight = 0
    this.screenHistory = [{ at: now, screen: this.currentScreen }]
  }

  setScreen(screen: PerformanceScreen) {
    if (!screens.has(screen) || screen === this.currentScreen)
      return
    this.currentScreen = screen
    this.screenHistory.push({ at: this.clock(), screen })
    if (this.screenHistory.length > 32)
      this.screenHistory.shift()
  }

  capture(): MeasurementScope | null {
    return this.active && this.id ? { epoch: this.epoch, screen: this.currentScreen } : null
  }

  private current(scope: MeasurementScope | null): scope is MeasurementScope {
    return !!scope && this.active && !!this.id && scope.epoch === this.epoch && screens.has(scope.screen)
  }

  begin(operation: PerformanceOperation, scope = this.capture()) {
    if (!operations.has(operation) || !this.current(scope))
      return null
    const at = this.clock()
    const token = this.nextPending++
    const tracked = this.pending.size < 64
    if (tracked)
      this.pending.set(token, { operation, scope, at })
    else
      this.omittedInFlight++
    this.sequence++
    let finished = false
    return (outcome: PerformanceOutcome = 'ok') => {
      if (finished)
        return
      finished = true
      if (!this.current(scope))
        return
      if (tracked)
        this.pending.delete(token)
      else
        this.omittedInFlight--
      this.record(operation, scope, this.clock() - at, outcome)
    }
  }

  measure<T>(operation: PerformanceOperation, block: () => T, scope = this.capture()): T {
    if (!operations.has(operation) || !this.current(scope))
      return block()
    const at = this.clock()
    let outcome: PerformanceOutcome = 'error'
    try {
      const value = block()
      outcome = 'ok'
      return value
    }
    finally {
      this.record(operation, scope, this.clock() - at, outcome)
    }
  }

  private record(operation: PerformanceOperation, scope: MeasurementScope, duration: number, outcome: PerformanceOutcome) {
    if (!this.current(scope) || !Number.isFinite(duration) || duration < 0)
      return
    const key = `${operation}:${scope.screen}`
    const result = outcome === 'ok' || outcome === 'cancelled' ? outcome : 'error'
    const stat = this.timings.get(key) ?? {
      ...distribution(),
      operation,
      screen: scope.screen,
      errors: 0,
      cancelled: 0,
    }
    if (!add(stat, duration))
      return
    if (result === 'error')
      stat.errors++
    if (result === 'cancelled')
      stat.cancelled++
    this.timings.set(key, stat)
    this.sequence++
    this.sample(scope.screen, operation, duration, result)
  }

  setProbeScheduler(schedule?: (reason: ProbeReason) => void) {
    this.probeScheduler = schedule
  }

  probe(reason: ProbeReason) {
    if (this.active)
      this.probeScheduler?.(reason)
  }

  recordProbe(scope: MeasurementScope | null, reason: ProbeReason, duration: number) {
    if (!this.current(scope) || !Number.isFinite(duration) || duration < 0 || !['INTERACTION', 'API', 'NAVIGATION'].includes(reason))
      return
    const key = `${reason}:${scope.screen}`
    const stat = this.probes.get(key) ?? {
      ...distribution(),
      screen: scope.screen,
      reason,
      late: 0,
    }
    if (!add(stat, duration))
      return
    if (duration >= 50)
      stat.late++
    this.probes.set(key, stat)
    this.sequence++
    this.sample(scope.screen, 'EVENT_LOOP_DELAY', duration, 'ok')
  }

  setLongTaskSupport(supported: boolean) {
    this.longTaskSupported = supported === true
  }

  recordLongTask(at: number, duration: number) {
    if (!this.active || !this.id || !this.longTaskSupported || !Number.isFinite(at) || !Number.isFinite(duration) || duration < 50 || at < this.activeSince || at < this.started || at > this.clock())
      return
    const screen = this.screenHistory.findLast(item => item.at <= at)?.screen ?? 'OTHER'
    const stat = this.longTasks.get(screen) ?? distribution()
    if (!add(stat, duration))
      return
    this.longTasks.set(screen, stat)
    this.sequence++
    this.sample(screen, 'LONG_TASK', duration, 'ok')
  }

  private sample(screen: PerformanceScreen, kind: SlowSample['kind'], duration: number, outcome: PerformanceOutcome) {
    if (duration < 50)
      return
    this.slow.push({
      elapsedMs: Math.max(0, Math.round(this.clock() - this.started)),
      screen,
      kind,
      durationMs: Math.min(duration, maximumDuration),
      outcome,
    })
    if (this.slow.length > 40)
      this.slow.shift()
  }

  report(build: WebBuild, connection: PerformanceConnection) {
    const validBuild = /^[a-f0-9]{64}$/.test(build.buildId) || (build.buildType === 'debug' && build.buildId === 'development')
    if (!this.id || !validBuild || !['debug', 'release'].includes(build.buildType) || !browsers.has(build.browser) || !connections.has(connection))
      return null
    // Hung requests age even without completions. Idle completed windows stay deduplicated.
    if (this.pending.size || this.omittedInFlight)
      this.sequence++
    const now = this.clock()
    const elapsedMs = Math.max(0, Math.round(now - this.started))
    const timings: Timing[] = []
    const apiFailures: Array<{
      operation: PerformanceOperation
      screen: PerformanceScreen
      errors: number
      cancelled: number
    }> = []
    for (const stat of this.timings.values()) {
      timings.push({
        operation: stat.operation,
        screen: stat.screen,
        count: stat.count,
        startedOnMainThread: stat.count,
        meanMs: Math.min(stat.maximum, stat.total / stat.count),
        maxMs: stat.maximum,
        p95UpperBoundMs: percentile(stat),
      })
      if ((stat.operation === 'API_READ' || stat.operation === 'API_WRITE') && (stat.errors || stat.cancelled)) {
        apiFailures.push({
          operation: stat.operation,
          screen: stat.screen,
          errors: stat.errors,
          cancelled: stat.cancelled,
        })
      }
    }

    const pending = new Map<string, {
      operation: PerformanceOperation
      screen: PerformanceScreen
      count: number
      maxElapsedMs: number
    }>()
    for (const value of this.pending.values()) {
      const key = `${value.operation}:${value.scope.screen}`
      const stat = pending.get(key) ?? {
        operation: value.operation,
        screen: value.scope.screen,
        count: 0,
        maxElapsedMs: 0,
      }
      stat.count++
      stat.maxElapsedMs = Math.max(stat.maxElapsedMs, Math.min(maximumDuration, Math.max(0, now - value.at)))
      pending.set(key, stat)
    }

    return {
      schema: 2 as const,
      platform: 'web' as const,
      buildId: build.buildId,
      buildType: build.buildType,
      browser: build.browser,
      connection,
      measurements: {
        windowId: this.id,
        sequence: this.sequence,
        elapsedMs,
        foregroundMs: Math.min(elapsedMs, Math.max(0, Math.round(this.foreground + (this.active ? now - this.activeSince : 0)))),
        operations: timings,
        apiFailures,
        inFlight: [...pending.values()],
        omittedInFlight: this.omittedInFlight,
        eventLoop: [...this.probes.values()].map(stat => ({
          screen: stat.screen,
          reason: stat.reason,
          count: stat.count,
          late: stat.late,
          meanDelayMs: Math.min(stat.maximum, stat.total / stat.count),
          maxDelayMs: stat.maximum,
          p95UpperBoundDelayMs: percentile(stat),
        })),
        longTaskSupported: this.longTaskSupported,
        longTasks: [...this.longTasks.entries()].map(([screen, stat]) => ({
          screen,
          count: stat.count,
          totalMs: stat.total,
          maxMs: stat.maximum,
        })),
        recentSlowSamples: this.slow.map(sample => ({ ...sample })),
      },
    }
  }
}

export type WebPerformanceReport = NonNullable<ReturnType<ClientPerformance['report']>>
export const clientPerformance = new ClientPerformance()
