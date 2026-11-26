// Mermaid diagrams in Markdown fences, parsed into a small model that both the web and
// Android renderers lay out natively. Keep this module in step with Android's Diagram*.kt:
// tests/fixtures/diagrams.json pins the parse results of both implementations and
// packages/contracts/diagram-spec.json is the single source of truth for limits and layout constants.

import type { Diagram } from './diagram/types'
import { statements } from './diagram/common'
import { parseFlowchart, parseState } from './diagram/flowchart'
import { parsePie, parseSequence } from './diagram/sequence-pie'
import { DIAGRAM_LIMITS, DiagramError } from './diagram/types'

export { cleanLabel } from './diagram/common'
export * from './diagram/types'

/** Fenced code languages rendered as a diagram. */
export function isDiagramLanguage(language: string | undefined) {
  return language === 'mermaid' || language === 'mmd'
}

/** Diagram kinds this renderer draws; others keep their source block. */
export function diagramKind(source: string): 'flowchart' | 'state' | 'sequence' | 'pie' | undefined {
  const header = statements(source).header?.toLowerCase() ?? ''
  if (/^(?:flowchart|graph)(?:-elk)?\b/.test(header))
    return 'flowchart'
  if (/^statediagram(?:-v2)?\b/.test(header))
    return 'state'
  if (/^sequencediagram\b/.test(header))
    return 'sequence'
  if (/^pie\b/.test(header))
    return 'pie'
  return undefined
}

export function parseDiagram(source: string): Diagram {
  if (source.length > DIAGRAM_LIMITS.source)
    throw new DiagramError('This diagram is too large to draw.')
  const kind = diagramKind(source)
  if (kind === 'flowchart')
    return parseFlowchart(source)
  if (kind === 'state')
    return parseState(source)
  if (kind === 'sequence')
    return parseSequence(source)
  if (kind === 'pie')
    return parsePie(source)
  throw new DiagramError('This diagram type is not supported.')
}
