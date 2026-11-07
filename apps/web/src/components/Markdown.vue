<script setup lang="ts">
import type { Deliverable } from '../../../../packages/contracts/artifacts'
import DOMPurify from 'dompurify'
import { marked, Renderer } from 'marked'
import {
  computed,
  defineAsyncComponent,
  onBeforeUnmount,
  ref,
} from 'vue'
import { api, apiResourceUrl } from '../api'
import { clientPerformance } from '../client-performance'
import { artifactLink } from '../deliverables'
import { isDiagramLanguage } from '../diagram'
import { DiagramCache, diagramFigure, diagramFonts } from '../diagram-svg'
import { HighlightCache } from '../highlight-cache'
import { Check, Copy } from '../icons'
import { markdownHtml as vMarkdownHtml } from '../markdown-html'

const props = defineProps<{ content: string, compactLinks?: boolean }>()
const ArtifactViewer = defineAsyncComponent(() => import('./ArtifactViewer.vue'))
const DiagramViewer = defineAsyncComponent(() => import('./DiagramViewer.vue'))
const opening = ref<{ items: Deliverable[], id: string }>()
const viewing = ref<{ source: string, returnFocus: HTMLElement }>()
const loading = ref(false)
const error = ref('')
const highlights = new HighlightCache()
const diagrams = new DiagramCache()
// Sources whose figure shows its code; re-renders while streaming keep the reader's choice.
const sourceViews = new Set<string>()
let request: AbortController | undefined
let copied: ReturnType<typeof setTimeout> | undefined
onBeforeUnmount(() => {
  request?.abort()
  clearTimeout(copied)
})

function diagramAction(event: MouseEvent) {
  const button = event.target instanceof Element ? event.target.closest<HTMLButtonElement>('button[data-diagram-action]') : null
  const figure = button?.closest<HTMLElement>('figure.diagram')
  if (!button || !figure)
    return false
  const source = figure.querySelector('.diagram-source code')?.textContent ?? ''
  const kind = button.dataset.diagramAction
  if (kind === 'source') {
    const showing = figure.classList.toggle('diagram-showing-source')
    button.setAttribute('aria-pressed', String(showing))
    if (showing)
      sourceViews.add(source)
    else
      sourceViews.delete(source)
  }
  else if (kind === 'copy') {
    void navigator.clipboard?.writeText(source).then(() => {
      const icon = button.querySelector('i')
      icon?.classList.replace(Copy, Check)
      button.title = 'Copied'
      clearTimeout(copied)
      copied = setTimeout(() => {
        icon?.classList.replace(Check, Copy)
        button.title = 'Copy source'
      }, 1500)
    })
  }
  else if (kind === 'expand') {
    error.value = ''
    viewing.value = { source, returnFocus: button }
  }

  return true
}

function onClick(event: MouseEvent) {
  if (!diagramAction(event))
    void openLink(event)
}

async function openLink(event: MouseEvent) {
  if (event.defaultPrevented || event.button !== 0 || event.ctrlKey || event.metaKey || event.shiftKey || event.altKey)
    return
  const anchor = event.target instanceof Element ? event.target.closest('a[href]') : null
  const target = anchor && artifactLink(anchor.getAttribute('href')!, window.location.origin)
  if (!target)
    return
  event.preventDefault()
  request?.abort()
  const controller = new AbortController()
  request = controller
  loading.value = true
  error.value = ''
  try {
    const items = await api<Deliverable[]>(`/runs/${target.runId}/artifacts`, { signal: controller.signal })
    if (!items.some(item => item.id === target.id && item.runId === target.runId))
      throw new Error('This file is no longer available.')
    if (!controller.signal.aborted)
      opening.value = { items, id: target.id }
  }
  catch (e) {
    if (!controller.signal.aborted)
      error.value = (e as Error).message
  }
  finally {
    if (!controller.signal.aborted)
      loading.value = false
  }
}

/** A real closing delimiter is present in raw Markdown but excluded from its parsed literal. */
function closedFence(raw: string, literal: string) {
  const opening = /^ {0,3}(`{3,}|~{3,})/.exec(raw)?.[1]
  if (!opening)
    return false
  const trailingFences = (text: string) => {
    let count = 0
    let end = text.length
    while (end > 0) {
      const start = text.lastIndexOf('\n', end - 1)
      const line = text.slice(start + 1, end).replace(/^[ \t>]*/, '').trim()
      end = start
      if (!line)
        continue
      if (line.length < opening.length || line !== opening[0]!.repeat(line.length))
        break
      count++
    }

    return count
  }

  // Literal lines may themselves end with indented ticks; count the entire trailing run.
  return trailingFences(raw) > trailingFences(literal)
}

const html = computed(() => clientPerformance.measure('MARKDOWN', () => {
  let rawHtml = false
  // Streaming fences stay code until their closing fence arrives, so drawings never flicker.
  const codeRenderer = new Renderer()
  const closedFenceAttribute = `data-diagram-${crypto.randomUUID()}`
  let hasDiagrams = false
  const renderCode = codeRenderer.code.bind(codeRenderer)
  codeRenderer.code = (token) => {
    const rendered = renderCode(token)
    if (!isDiagramLanguage(token.lang?.split(/\s/)[0]) || !closedFence(token.raw, token.text))
      return rendered
    hasDiagrams = true
    return rendered.replace('<pre>', `<pre ${closedFenceAttribute}>`)
  }

  const parsed = marked.parse(props.content, {
    async: false,
    renderer: codeRenderer,
    walkTokens(token) {
      if (token.type === 'html')
        rawHtml = true
      // Sync parsing needs no callback results. Avoid growing the result array
      // that Marked concatenates at each token while collecting async work.
      return []
    },
  }) as string
  const container = document.createElement('div')
  container.innerHTML = DOMPurify.sanitize(parsed)
  highlights.beginRender()
  if (hasDiagrams) {
    // Layouts measured before the web fonts loaded are redone once they arrive.
    void diagramFonts.value
    diagrams.beginRender()
  }

  const diagramFigures = new Set<Element>()

  for (const code of container.querySelectorAll('pre code')) {
    const language = [...code.classList].find(name => name.startsWith('language-'))?.slice(9)
    const source = (code.textContent || '').replace(/\n$/, '')
    const closed = code.parentElement!.hasAttribute(closedFenceAttribute)
    code.parentElement!.removeAttribute(closedFenceAttribute)
    const figure = isDiagramLanguage(language) && closed && diagramFigure(source, diagrams, sourceViews.has(source))
    if (figure) {
      diagramFigures.add(figure)
      code.parentElement!.replaceWith(figure)
      continue
    }

    code.innerHTML = highlights.render(code.textContent || '', language)
  }

  for (const table of container.querySelectorAll('table')) {
    const scroll = document.createElement('div')
    scroll.className = 'markdown-table'
    table.replaceWith(scroll)
    scroll.append(table)
  }

  if (props.compactLinks) {
    // Reports often contain bare artifact paths. Link only this known route,
    // leaving existing links and code untouched, and retain the viewer handler.
    const walker = document.createTreeWalker(container, NodeFilter.SHOW_TEXT)
    const nodes: Text[] = []
    while (walker.nextNode()) {
      const parent = walker.currentNode.parentElement
      const figure = parent?.closest('figure.diagram')
      if (!parent?.closest('a, code, pre') && (!figure || !diagramFigures.has(figure)))
        nodes.push(walker.currentNode as Text)
    }

    for (const node of nodes) {
      const matches = [...node.data.matchAll(/(?<![\w/])\/api\/runs\/[\w-]+\/artifacts\/[\w-]+/g)]
      if (!matches.length)
        continue
      const fragment = document.createDocumentFragment()
      let offset = 0
      for (const match of matches) {
        fragment.append(node.data.slice(offset, match.index))
        const link = document.createElement('a')
        link.setAttribute('href', match[0])
        link.textContent = match[0]
        fragment.append(link)
        offset = match.index + match[0].length
      }

      fragment.append(node.data.slice(offset))
      node.replaceWith(fragment)
    }
  }

  for (const link of container.querySelectorAll('a[href]')) {
    link.setAttribute('target', '_blank')
    link.setAttribute('rel', 'noopener noreferrer')
    const href = link.getAttribute('href')!
    if (artifactLink(href, window.location.origin))
      link.setAttribute('href', apiResourceUrl(href))
    const figure = link.closest('figure.diagram')
    const generated = figure !== null && diagramFigures.has(figure)
    if (props.compactLinks && !generated && link.textContent === href) {
      try {
        const url = new URL(href, window.location.href)
        if (!['https:', 'http:'].includes(url.protocol))
          continue
        const github = url.hostname === 'github.com'
        link.textContent = artifactLink(href, window.location.origin)
          ? 'View file'
          : github && /^\/[^/]+\/[^/]+\/actions\/runs\/\d+/.test(url.pathname)
            ? 'View workflow'
            : github && /^\/[^/]+\/[^/]+\/commit\//.test(url.pathname)
              ? 'View commit'
              : url.hostname
        link.setAttribute('title', href)
      }
      catch { /* Keep the original label for malformed URLs. */ }
    }
  }

  for (const image of container.querySelectorAll('img[src]'))
    image.setAttribute('src', apiResourceUrl(image.getAttribute('src')!))

  if (rawHtml) {
    // Existing link/table transformations can create nodes in parser-sensitive
    // contexts (SVG, textarea, etc.). Preserve their historical HTML round-trip.
    const normalized = document.createElement('div')
    normalized.innerHTML = container.innerHTML
    return normalized
  }

  return container
}))
</script>

<template>
  <div v-markdown-html="html" class="markdown text-sm leading-[1.8] wrap-anywhere" @click="onClick" />
  <p v-if="loading" role="status" class="text-sm text-muted">
    Opening file…
  </p>
  <p v-if="error" role="alert" class="text-sm text-danger">
    {{ error }}
  </p>
  <ArtifactViewer
    v-if="opening"
    :key="opening.id"
    :items="opening.items"
    :initial="opening.id"
    @close="opening = undefined"
  />
  <DiagramViewer
    v-if="viewing"
    :source="viewing.source"
    :return-focus="viewing.returnFocus"
    :opening-link="loading"
    :link-error="error"
    @open-link="openLink"
    @close="viewing = undefined"
  />
</template>
