import type { TimelineEntry } from '../../packages/contracts/live'
import {
  createApp,
  h,
  nextTick,
  ref,
  shallowRef,
} from 'vue'
import { activityEntries, ActivityGroups } from '../../apps/web/src/activity'
import ActivityFeed from '../../apps/web/src/components/ActivityFeed.vue'
import Markdown from '../../apps/web/src/components/Markdown.vue'
import { LiveTimeline } from '../../apps/web/src/live-timeline'
import '@fontsource-variable/dm-sans/wght.css'
import '@fontsource-variable/manrope/wght.css'
import '../../apps/web/src/styles/index.css'

type Workload = 'prose' | 'code' | 'paragraph'

interface Scenario {
  history: number
  characters: number
  workload: Workload
  component: 'feed' | 'markdown'
  rawProps?: boolean
}

function stats(values: number[]) {
  const sorted = [...values].sort((a, b) => a - b)
  return {
    samples: sorted.length,
    p50Ms: sorted[Math.ceil(sorted.length * 0.5) - 1],
    p95Ms: sorted[Math.ceil(sorted.length * 0.95) - 1],
    maxMs: sorted.at(-1),
  }
}

function source(workload: Workload, characters: number) {
  const fragments = {
    prose: '## Étape\n\nUne **réponse détaillée** avec du texte, une liste et un lien.\n\n- Premier point\n- Deuxième point\n\n',
    code: '## Implémentation\n\n```typescript\nexport function total(values: number[]): number {\n  return values.reduce((sum, value) => sum + value, 0)\n}\n```\n\nLe calcul est **terminé**.\n\n',
    paragraph: 'Une très longue réponse avec du **texte en gras** et une phrase supplémentaire. ',
  }
  return fragments[workload].repeat(Math.ceil(characters / fragments[workload].length)).slice(0, characters)
}

/** A timeline card as the server sends it: a person's message or the agent's reply. */
function event(id: number, text: string, user = false): TimelineEntry {
  const createdAt = 1_700_000_000_000 + id
  return {
    id: user ? `user:${id}` : `message:1:answer-${id}`,
    runId: 'measurement',
    position: id,
    revision: id,
    source: id,
    turn: 1,
    attempt: 0,
    createdAt,
    updatedAt: createdAt,
    content: user
      ? {
          kind: 'userMessage',
          text,
          sentAt: createdAt,
          attachments: [],
        }
      : {
          kind: 'agentMessage',
          itemId: `answer-${id}`,
          text,
          state: 'running',
        },
  }
}

const frame = () => new Promise<number>(resolve => requestAnimationFrame(resolve))

async function settle() {
  await nextTick()
  await frame()
  await new Promise(resolve => setTimeout(resolve, 0))
}

async function measure(scenario: Scenario) {
  const root = document.getElementById('app')!
  const prior = Array.from({ length: scenario.history }, (_, index) => event(
    index + 1,
    index % 2 ? source('prose', 300) : `Question ${index + 1} : peux-tu vérifier cette étape ?`,
    index % 2 === 0,
  ))
  const messageId = prior.length + 1
  let text = source(scenario.workload, scenario.characters)
  // Raw props isolate component work; the default includes useLiveRun's deep refs.
  const events = scenario.rawProps
    ? shallowRef([...prior, event(messageId, text)])
    : ref([...prior, event(messageId, text)])
  const content = ref(text)
  const mountTimes: number[] = []
  const updateTimes: number[] = []
  const layoutTimes: number[] = []
  let app: ReturnType<typeof createApp> | undefined
  const mount = () => {
    root.style.cssText = 'height:100dvh;display:flex;flex-direction:column;max-width:1100px;margin:auto;overflow:auto'
    app = createApp({
      render: () => scenario.component === 'markdown'
        ? h(Markdown, { content: content.value })
        : h(ActivityFeed, {
            events: [],
            timeline: events.value,
            active: true,
            agent: 'Synthetic agent',
            task: 'Measurement',
            more: false,
            loading: false,
            trimmed: 0,
            chat: true,
            compactToolbar: true,
          }),
    })
    app.mount(root)
  }

  // Fresh component mounts, with the module graph and JS engine already warm.
  for (let index = 0; index < 6; index++) {
    const start = performance.now()
    mount()
    root.getBoundingClientRect()
    const duration = performance.now() - start
    await settle()
    if (index > 0)
      mountTimes.push(duration)
    app!.unmount()
  }

  mount()
  await settle()
  const stable = [...root.querySelectorAll('.markdown')].at(-1)?.querySelector('p')?.firstChild
  const originalStable = stable
  const longTasks: number[] = []
  const observer = new PerformanceObserver((list) => {
    for (const item of list.getEntries())
      longTasks.push(item.duration)
  })
  observer.observe({ type: 'longtask' })
  for (let index = 0; index < 40; index++) {
    text += ` suite ${index}`
    const start = performance.now()
    content.value = text
    events.value = [...prior, event(messageId, text)]
    await nextTick()
    const updated = performance.now()
    root.getBoundingClientRect()
    root.querySelector('.activity-scroll')?.getBoundingClientRect()
    const laidOut = performance.now()
    await settle()
    if (index >= 10) {
      updateTimes.push(updated - start)
      layoutTimes.push(laidOut - updated)
    }
  }

  observer.disconnect()
  const result = {
    ...scenario,
    mount: stats(mountTimes),
    update: stats(updateTimes),
    layout: stats(layoutTimes),
    longTasksIncludingWarmup: longTasks,
    nodes: root.querySelectorAll('*').length,
    messages: root.querySelectorAll('.activity-message').length,
    prefixNodeRetained: originalStable?.isConnected ?? false,
    finalTextPresent: root.textContent?.includes('suite 39') ?? false,
  }
  if (!result.finalTextPresent)
    throw new Error('Stream did not deliver final text')
  app!.unmount()
  return result
}

async function scroll(history: number) {
  const root = document.getElementById('app')!
  const events = Array.from({ length: history }, (_, index) => event(index + 1, source('prose', 300), index % 2 === 0))
  root.style.cssText = 'height:100dvh;display:flex;flex-direction:column'
  const app = createApp({
    render: () => h(ActivityFeed, {
      events: [],
      timeline: events,
      active: false,
      agent: 'Synthetic',
      task: 'Scroll',
      more: false,
      loading: false,
      trimmed: 0,
      chat: true,
    }),
  })
  app.mount(root)
  await settle()
  const scroller = root.querySelector<HTMLElement>('.activity-scroll')!
  const times = []
  let previous = await frame()
  for (let index = 0; index < 120; index++) {
    scroller.scrollTop = (scroller.scrollHeight - scroller.clientHeight) * (1 - index / 119)
    const now = await frame()
    times.push(now - previous)
    previous = now
  }

  const result = {
    history,
    frameIntervals: stats(times),
    over25Ms: times.filter(time => time > 25).length,
    totalFrames: times.length,
    nodes: root.querySelectorAll('*').length,
  }
  app.unmount()
  return result
}

function algorithms(count: number) {
  const events = Array.from({ length: count }, (_, index) => event(index + 1, `Question ${index + 1}`, index % 2 === 0))
  const groups = new ActivityGroups()
  const accumulator = new LiveTimeline()
  accumulator.restore([], count, events)
  const reply = event(count + 1, '')
  accumulator.apply([], [{ kind: 'upsert', entry: reply }])
  const times = []
  const reduction = []
  for (let index = 0; index < 120; index++) {
    const start = performance.now()
    // A streaming reply appends its new text to the card this stream delivered.
    accumulator.apply([], [{
      kind: 'append',
      id: reply.id,
      revision: count + index + 2,
      source: count + index + 2,
      updatedAt: reply.createdAt + index,
      text: ` Suite ${index}`,
    }])
    const cards = accumulator.entries()
    const reduced = performance.now()
    activityEntries([], true, groups, undefined, cards)
    const done = performance.now()
    if (index >= 20) {
      reduction.push(reduced - start)
      times.push(done - reduced)
    }
  }

  return { events: count, accumulator: stats(reduction), timeline: stats(times) }
}

let markdownApp: ReturnType<typeof createApp> | undefined
const markdownText = ref('')
const compactLinks = ref(false)

async function markdown(content: string, compact = false) {
  markdownText.value = content
  compactLinks.value = compact
  if (!markdownApp) {
    const root = document.getElementById('app')!
    root.style.cssText = 'padding:16px;max-width:900px'
    markdownApp = createApp({ render: () => h(Markdown, { content: markdownText.value, compactLinks: compactLinks.value }) })
    markdownApp.mount(root)
  }

  await settle()
}

async function freshMarkdownHtml(content: string, compact = false) {
  const root = document.createElement('div')
  document.body.append(root)
  const app = createApp({ render: () => h(Markdown, { content, compactLinks: compact }) })
  try {
    app.mount(root)
    await nextTick()
    return root.querySelector('.markdown')!.innerHTML
  }
  finally {
    app.unmount()
    root.remove()
  }
}

const harness = {
  measure,
  scroll,
  algorithms,
  markdown,
  freshMarkdownHtml,
}

declare global {
  interface Window {
    chatRendering: typeof harness
  }
}

Object.assign(window, { chatRendering: harness })
