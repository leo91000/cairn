// Mermaid diagrams in Markdown fences, parsed into a small model that both the web and
// Android renderers lay out natively. Keep this file in step with Android's Diagram.kt:
// tests/fixtures/diagrams.json pins the parse results of both implementations.

export type Direction = 'TB' | 'BT' | 'LR' | 'RL'

export type NodeShape
  = | 'rect'
    | 'round'
    | 'stadium'
    | 'subroutine'
    | 'cylinder'
    | 'circle'
    | 'doublecircle'
    | 'diamond'
    | 'hexagon'
    | 'parallelogram'
    | 'parallelogram-alt'
    | 'trapezoid'
    | 'trapezoid-alt'
    | 'flag'
    | 'note'
    | 'start'
    | 'end'
    | 'fork'
    | 'choice'

/** Author colors are kept only when they are plain CSS colors. */
export interface DiagramStyle {
  fill?: string
  stroke?: string
  color?: string
  strokeWidth?: number
  dashed?: boolean
}

export interface GraphNode {
  id: string
  label: string
  shape: NodeShape
  parent?: string
  style?: DiagramStyle
  link?: string
}

export type EdgeLine = 'solid' | 'dotted' | 'thick' | 'invisible'
export type EdgeHead = 'none' | 'arrow' | 'circle' | 'cross'

export interface GraphEdge {
  from: string
  to: string
  label: string
  line: EdgeLine
  start: EdgeHead
  end: EdgeHead
  /** Extra dashes request a longer edge, as in Mermaid. */
  length: number
  style?: DiagramStyle
}

export interface GraphCluster {
  id: string
  label: string
  parent?: string
  style?: DiagramStyle
}

export interface GraphDiagram {
  kind: 'graph'
  title?: string
  direction: Direction
  nodes: GraphNode[]
  edges: GraphEdge[]
  clusters: GraphCluster[]
}

export interface Participant {
  id: string
  label: string
  actor: boolean
}

export type MessageHead = 'arrow' | 'open' | 'cross' | 'async' | 'none'

export type SequenceItem
  = | {
    type: 'message'
    from: string
    to: string
    label: string
    line: 'solid' | 'dotted'
    head: MessageHead
    both: boolean
    activate: boolean
    deactivate: boolean
    number?: number
  }
  | {
    type: 'note'
    placement: 'left' | 'right' | 'over'
    participants: string[]
    label: string
  }
  | { type: 'activate' | 'deactivate', participant: string }
  | {
    type: 'block'
    kind: BlockKind
    label: string
    color?: string
  }
  | { type: 'branch', kind: 'else' | 'and' | 'option', label: string }
  | { type: 'end' }

export type BlockKind = 'loop' | 'alt' | 'opt' | 'par' | 'critical' | 'break' | 'rect'

export interface SequenceDiagram {
  kind: 'sequence'
  title?: string
  participants: Participant[]
  items: SequenceItem[]
}

export interface PieDiagram {
  kind: 'pie'
  title?: string
  showData: boolean
  slices: { label: string, value: number }[]
}

export type Diagram = GraphDiagram | SequenceDiagram | PieDiagram

export class DiagramError extends Error {}

export const DIAGRAM_LIMITS = {
  source: 20_000,
  nodes: 250,
  edges: 500,
  participants: 40,
  items: 600,
  slices: 40,
}

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

interface Statements {
  title?: string
  header?: string
  lines: string[]
}

/** Remove front matter, directives and comments; return the header and following lines. */
function statements(source: string): Statements {
  let lines = source.replace(/\r\n?/g, '\n').split('\n')
  let title: string | undefined
  const first = lines.findIndex(line => line.trim())
  if (first >= 0 && lines[first]!.trim() === '---') {
    const close = lines.findIndex((line, index) => index > first && line.trim() === '---')
    if (close > 0) {
      for (const line of lines.slice(first + 1, close)) {
        const trimmed = line.trim()
        if (trimmed.startsWith('title:') && trimmed.length > 6)
          title = unquote(trimmed.slice(6).trim())
      }

      lines = lines.slice(close + 1)
    }
  }

  const kept: string[] = []
  for (const raw of lines) {
    const line = raw.trim()
    if (!line || line.startsWith('%%'))
      continue
    kept.push(line)
  }

  const header = kept.shift()
  return { title, header, lines: kept }
}

function unquote(value: string) {
  const trimmed = value.trim()
  if (trimmed.length >= 2 && ((trimmed.startsWith('"') && trimmed.endsWith('"')) || (trimmed.startsWith('\'') && trimmed.endsWith('\''))))
    return trimmed.slice(1, -1)
  return trimmed
}

/** The first word of a statement and the trimmed rest. Plain scanning keeps parsing linear. */
function head(line: string): [string, string] {
  const space = line.search(/\s/)
  return space < 0 ? [line, ''] : [line.slice(0, space), line.slice(space).trim()]
}

/** Text before and after the first colon, both trimmed; `after` is undefined without one. */
function colon(text: string): [string, string | undefined] {
  const index = text.indexOf(':')
  return index < 0 ? [text.trim(), undefined] : [text.slice(0, index).trim(), text.slice(index + 1).trim()]
}

const ENTITIES: Record<string, string> = {
  'amp': '&',
  'lt': '<',
  'gt': '>',
  'quot': '"',
  'apos': '\'',
  'nbsp': ' ',
  '#39': '\'',
}

function entity(name: string) {
  const key = name.toLowerCase()
  return Object.hasOwn(ENTITIES, key) ? ENTITIES[key] : undefined
}

/** Mermaid labels allow entity codes, <br> breaks, simple tags and Markdown strings. */
export function cleanLabel(value: string) {
  let label = unquote(value)
  if (label.startsWith('`') && label.endsWith('`') && label.length >= 2)
    label = label.slice(1, -1)
  return label
    .replace(/<br\s*\/?>/gi, '\n')
    .replace(/\\n/g, '\n')
    .replace(/<\/?[a-z][^<>]*>/gi, '')
    .replace(/(?<!&)#(\d+);/g, (_, code: string) => String.fromCodePoint(Number(code)))
    .replace(/(?<!&)#([a-z]+);/gi, (match, name: string) => entity(name) ?? match)
    .replace(/&(#?\w+);/g, (match, name: string) => {
      if (/^#\d+$/.test(name))
        return String.fromCodePoint(Number(name.slice(1)))
      if (/^#x[\da-f]+$/i.test(name))
        return String.fromCodePoint(Number.parseInt(name.slice(2), 16))
      return entity(name) ?? match
    })
    .replace(/\bfa[bsr]?:fa-[\w-]+\s*/g, '')
    .replace(/\*\*(.+?)\*\*/g, '$1')
    .split('\n')
    .map(line => line.trim())
    .join('\n')
    .trim()
}

const COLOR = /^(?:#[\da-f]{3,8}|(?:rgba?|hsla?)\([\d\s.,%/-]+\)|[a-z]{3,20})$/i

function parseStyle(value: string): DiagramStyle | undefined {
  const style: DiagramStyle = {}
  for (const part of splitStyle(value)) {
    const separator = part.indexOf(':')
    if (separator < 0)
      continue
    const key = part.slice(0, separator).trim().toLowerCase()
    const raw = part.slice(separator + 1).replace(/!important/i, '').trim()
    if ((key === 'fill' || key === 'background' || key === 'background-color') && COLOR.test(raw)) {
      style.fill = raw
    }
    else if (key === 'stroke' && COLOR.test(raw)) {
      style.stroke = raw
    }
    else if (key === 'color' && COLOR.test(raw)) {
      style.color = raw
    }
    else if (key === 'stroke-width') {
      const width = Number.parseFloat(raw)
      if (Number.isFinite(width) && width > 0)
        style.strokeWidth = Math.min(width, 6)
    }
    else if (key === 'stroke-dasharray') {
      style.dashed = raw !== '0' && raw !== 'none'
    }
  }

  return Object.keys(style).length ? style : undefined
}

// Split properties outside color functions, preserving valid legacy and modern CSS syntax.
function splitStyle(value: string) {
  const parts: string[] = []
  let start = 0
  let depth = 0
  for (let index = 0; index < value.length; index++) {
    const character = value[index]
    if (character === '(') {
      depth++
    }
    else if (character === ')') {
      depth = Math.max(0, depth - 1)
    }
    else if (!depth && (character === ',' || character === ';')) {
      parts.push(value.slice(start, index))
      start = index + 1
    }
  }

  parts.push(value.slice(start))
  return parts
}

function mergeStyle(...styles: (DiagramStyle | undefined)[]) {
  const merged = Object.assign({}, ...styles.filter(Boolean)) as DiagramStyle
  return Object.keys(merged).length ? merged : undefined
}

function safeLink(value: string) {
  try {
    const url = new URL(unquote(value))
    return url.protocol === 'https:' || url.protocol === 'http:' ? url.href : undefined
  }
  catch {
    return undefined
  }
}

function direction(value: string | undefined): Direction {
  const upper = value?.toUpperCase()
  if (upper === 'LR' || upper === 'RL' || upper === 'BT')
    return upper
  return 'TB'
}

class GraphBuilder {
  nodes = new Map<string, GraphNode>()
  edges: GraphEdge[] = []
  clusters = new Map<string, GraphCluster>()
  classes = new Map<string, DiagramStyle>()
  nodeClasses = new Map<string, string[]>()
  nodeStyles = new Map<string, DiagramStyle>()
  linkStyles = new Map<number | 'default', DiagramStyle>()

  node(id: string, parent: string | undefined, shape: NodeShape = 'rect', label?: string) {
    let node = this.nodes.get(id)
    if (!node) {
      if (this.nodes.size >= DIAGRAM_LIMITS.nodes)
        throw new DiagramError('This diagram has too many nodes to draw.')
      node = { id, label: id, shape }
      this.nodes.set(id, node)
    }

    if (parent && !node.parent && !this.clusters.has(id))
      node.parent = parent
    if (label !== undefined) {
      node.label = label
      node.shape = shape
    }

    return node
  }

  edge(edge: GraphEdge) {
    if (this.edges.length >= DIAGRAM_LIMITS.edges)
      throw new DiagramError('This diagram has too many links to draw.')
    this.edges.push(edge)
  }

  addClass(ids: string, name: string) {
    for (const id of ids.split(',').map(value => value.trim()).filter(Boolean))
      this.nodeClasses.set(id, [...(this.nodeClasses.get(id) ?? []), name])
  }

  build(title: string | undefined, dir: Direction): GraphDiagram {
    // Reusing a cluster ID can reparent an existing ancestor. Reject cycles before layout.
    const checked = new Set<string>()
    for (const id of this.clusters.keys()) {
      const ancestors = new Set<string>()
      let current: string | undefined = id
      while (current && !checked.has(current)) {
        if (ancestors.has(current))
          throw new DiagramError('Subgraphs cannot contain themselves.')
        ancestors.add(current)
        current = this.clusters.get(current)?.parent
      }

      for (const ancestor of ancestors)
        checked.add(ancestor)
    }

    const nodes = [...this.nodes.values()].filter(node => !this.clusters.has(node.id))
    for (const node of nodes) {
      const classes = (this.nodeClasses.get(node.id) ?? []).map(name => this.classes.get(name))
      const style = mergeStyle(this.classes.get('default'), ...classes, this.nodeStyles.get(node.id))
      if (style)
        node.style = style
    }

    for (const cluster of this.clusters.values()) {
      const classes = (this.nodeClasses.get(cluster.id) ?? []).map(name => this.classes.get(name))
      const style = mergeStyle(...classes, this.nodeStyles.get(cluster.id))
      if (style)
        cluster.style = style
    }

    this.edges.forEach((edge, index) => {
      const style = mergeStyle(this.linkStyles.get('default'), this.linkStyles.get(index))
      if (style)
        edge.style = style
    })

    const known = new Set([...nodes.map(node => node.id), ...this.clusters.keys()])
    const edges = this.edges.filter(edge => known.has(edge.from) && known.has(edge.to))
    const graph: GraphDiagram = {
      kind: 'graph',
      direction: dir,
      nodes,
      edges,
      clusters: [...this.clusters.values()],
    }
    if (title)
      graph.title = title
    return graph
  }

  /** Shared styling statements; returns false when the line is something else. */
  styling(line: string) {
    const [keyword, rest] = head(line.replace(/;$/, ''))
    if (keyword === 'classDef' || keyword === 'style' || keyword === 'linkStyle') {
      const [target, body] = head(rest)
      if (!body)
        return false
      const style = parseStyle(body)
      if (keyword === 'classDef') {
        for (const name of target.split(','))
          this.classes.set(name.trim(), mergeStyle(this.classes.get(name.trim()), style) ?? {})
      }
      else if (keyword === 'style') {
        if (style)
          this.nodeStyles.set(target, mergeStyle(this.nodeStyles.get(target), style)!)
      }
      else {
        for (const index of target.split(',')) {
          const key = index.trim() === 'default' ? 'default' : Number(index)
          if (style && (key === 'default' || Number.isInteger(key)))
            this.linkStyles.set(key, style)
        }
      }

      return true
    }

    if (keyword === 'class') {
      const space = rest.lastIndexOf(' ')
      if (space < 0)
        return false
      this.addClass(rest.slice(0, space), rest.slice(space + 1))
      return true
    }

    return keyword === 'accTitle' || keyword === 'accTitle:' || keyword === 'accDescr' || keyword === 'accDescr:' || keyword === 'interpolate'
  }
}

// Opening delimiters, longest first, with their closing delimiter and shape.
const SHAPES: [string, string, NodeShape][] = [
  ['(((', ')))', 'doublecircle'],
  ['((', '))', 'circle'],
  ['([', '])', 'stadium'],
  ['[[', ']]', 'subroutine'],
  ['[(', ')]', 'cylinder'],
  ['[/', '/]', 'parallelogram'],
  ['[\\', '\\]', 'parallelogram-alt'],
  ['{{', '}}', 'hexagon'],
  ['(', ')', 'round'],
  ['[', ']', 'rect'],
  ['{', '}', 'diamond'],
  ['>', ']', 'flag'],
]

const NAMED_SHAPES: Record<string, NodeShape> = {
  'rect': 'rect',
  'rectangle': 'rect',
  'proc': 'rect',
  'process': 'rect',
  'rounded': 'round',
  'event': 'round',
  'stadium': 'stadium',
  'pill': 'stadium',
  'terminal': 'stadium',
  'subproc': 'subroutine',
  'subroutine': 'subroutine',
  'cyl': 'cylinder',
  'cylinder': 'cylinder',
  'db': 'cylinder',
  'database': 'cylinder',
  'circle': 'circle',
  'circ': 'circle',
  'dbl-circ': 'doublecircle',
  'double-circle': 'doublecircle',
  'diam': 'diamond',
  'diamond': 'diamond',
  'decision': 'diamond',
  'question': 'diamond',
  'hex': 'hexagon',
  'hexagon': 'hexagon',
  'prepare': 'hexagon',
  'lean-r': 'parallelogram',
  'in-out': 'parallelogram',
  'lean-l': 'parallelogram-alt',
  'out-in': 'parallelogram-alt',
  'trap-b': 'trapezoid',
  'trapezoid': 'trapezoid',
  'priority': 'trapezoid',
  'trap-t': 'trapezoid-alt',
  'inv-trapezoid': 'trapezoid-alt',
  'manual': 'trapezoid-alt',
  'odd': 'flag',
  'flag': 'flag',
  'doc': 'note',
  'document': 'note',
  'notch-rect': 'note',
  'card': 'note',
  'sm-circ': 'start',
  'start': 'start',
  'fr-circ': 'end',
  'stop': 'end',
  'fork': 'fork',
  'join': 'fork',
}

const ID_STOP = /[\s[\](){}<>|&;:"'`,]/

class Scanner {
  index = 0
  constructor(readonly text: string) {}

  get done() {
    return this.index >= this.text.length
  }

  skipSpace() {
    while (this.index < this.text.length && /\s/.test(this.text[this.index]!))
      this.index++
  }

  startsWith(value: string) {
    return this.text.startsWith(value, this.index)
  }

  /** A link begins here, so an identifier must end. */
  atLink() {
    return /^(?:--|==|-\.|~~|<-|<=)/.test(this.text.slice(this.index, this.index + 2))
  }

  id() {
    const start = this.index
    while (!this.done && !ID_STOP.test(this.text[this.index]!) && !this.atLink() && !this.startsWith('@{'))
      this.index++
    return this.text.slice(start, this.index)
  }

  /** Text up to a closing delimiter; quoted text may contain the delimiter. */
  until(close: string) {
    this.skipSpace()
    if (this.startsWith('"')) {
      const end = this.text.indexOf('"', this.index + 1)
      if (end > 0) {
        const quoted = this.text.slice(this.index, end + 1)
        this.index = end + 1
        this.skipSpace()
        if (this.startsWith(close)) {
          this.index += close.length
          return quoted
        }

        const rest = this.text.indexOf(close, this.index)
        if (rest < 0)
          throw new DiagramError(`Missing “${close}” after a node label.`)
        const text = quoted + this.text.slice(this.index, rest)
        this.index = rest + close.length
        return text
      }
    }

    const end = this.text.indexOf(close, this.index)
    if (end < 0)
      throw new DiagramError(`Missing “${close}” after a node label.`)
    const text = this.text.slice(this.index, end)
    this.index = end + close.length
    return text
  }
}

interface NodeRef {
  id: string
  shape?: NodeShape
  label?: string
  classes: string[]
}

function readNode(scanner: Scanner): NodeRef | undefined {
  scanner.skipSpace()
  const id = scanner.id()
  if (!id)
    return undefined
  const node: NodeRef = { id, classes: [] }

  if (scanner.startsWith('@{')) {
    const end = scanner.text.indexOf('}', scanner.index)
    if (end < 0)
      throw new DiagramError('Missing “}” after node properties.')
    const body = scanner.text.slice(scanner.index + 2, end)
    scanner.index = end + 1
    const shape = /\bshape\s*:\s*([\w-]+)/.exec(body)?.[1]?.toLowerCase()
    const label = /\blabel\s*:\s*("(?:[^"\\]|\\.)*"|'[^']*'|[^,}]+)/.exec(body)?.[1]
    node.shape = (shape && Object.hasOwn(NAMED_SHAPES, shape) && NAMED_SHAPES[shape]) || 'rect'
    node.label = label === undefined ? id : cleanLabel(label)
  }
  else {
    for (const [open, close, shape] of SHAPES) {
      if (!scanner.startsWith(open))
        continue
      scanner.index += open.length
      if (shape === 'parallelogram' || shape === 'parallelogram-alt') {
        // [/text\] and [\text/] are trapezoids; find whichever slash closes the label.
        const slash = scanner.text.indexOf('/]', scanner.index)
        const backslash = scanner.text.indexOf('\\]', scanner.index)
        const ends = [slash, backslash].filter(index => index >= 0)
        if (!ends.length)
          throw new DiagramError(`Missing “${close}” after a node label.`)
        const end = Math.min(...ends)
        const closing = scanner.text.slice(end, end + 2)
        node.label = cleanLabel(scanner.text.slice(scanner.index, end))
        scanner.index = end + 2
        node.shape = shape === 'parallelogram'
          ? (closing === '/]' ? 'parallelogram' : 'trapezoid')
          : (closing === '\\]' ? 'parallelogram-alt' : 'trapezoid-alt')
      }
      else {
        node.label = cleanLabel(scanner.until(close))
        node.shape = shape
      }

      break
    }
  }

  while (scanner.startsWith(':::')) {
    scanner.index += 3
    const name = scanner.id()
    if (name)
      node.classes.push(name)
  }

  return node
}

interface LinkRef {
  line: EdgeLine
  start: EdgeHead
  end: EdgeHead
  length: number
  label: string
}

const HEADS: Record<string, EdgeHead> = {
  '>': 'arrow',
  '<': 'arrow',
  'o': 'circle',
  'x': 'cross',
}

/** Whether a link head at `index` ends there: `o` and `x` are heads only before a gap. */
function boundary(text: string, index: number) {
  return index >= text.length || /[\s|]/.test(text[index]!)
}

function run(text: string, from: number, char: string) {
  let end = from
  while (text[end] === char)
    end++
  return end - from
}

function headEnd(text: string, index: number) {
  if (text[index] === '>')
    return index + 1
  return (text[index] === 'o' || text[index] === 'x') && boundary(text, index + 1) ? index + 1 : -1
}

/** End of an unlabelled link body starting at `at` (`-->`, `---`, `-.->`, `==>`, `~~~`…), or -1. */
function linkBody(text: string, at: number) {
  const char = text[at]
  if (char === '~') {
    const count = run(text, at, '~')
    return count >= 3 ? at + count : -1
  }

  if (char === '-' && text[at + 1] === '.') {
    const close = at + 1 + run(text, at + 1, '.')
    if (text[close] !== '-')
      return -1
    const end = headEnd(text, close + 1)
    return end >= 0 ? end : close + 1
  }

  if (char === '-' || char === '=') {
    const count = run(text, at, char)
    const end = headEnd(text, at + count)
    if (count >= 2 && end >= 0)
      return end
    return count >= 3 ? at + count : -1
  }

  return -1
}

/** `-- text -->`, `== text ==>` and `-. text .->`: the label and the equivalent link body. */
function labelledLink(text: string, at: number) {
  const open = text.slice(at, at + 2)
  if ((open !== '--' && open !== '==' && open !== '-.') || !/\s/.test(text[at + 2] ?? ''))
    return undefined
  for (let index = at + 3; index < text.length; index++) {
    let end = -1
    let body = ''
    if (open === '-.') {
      if (text[index] === '.' && text[index - 1] !== '.') {
        const close = index + run(text, index, '.')
        if (text[close] === '-') {
          end = headEnd(text, close + 1)
          end = end >= 0 ? end : close + 1
          body = `-${text.slice(index, end)}`
        }
      }
    }
    else if (text[index] === open[0] && text[index - 1] !== open[0]) {
      end = linkBody(text, index)
      body = text.slice(index, end)
    }

    const label = text.slice(at + 2, index).trim()
    if (end > 0 && label)
      return { label, body, end }
  }

  return undefined
}

function readLink(scanner: Scanner): LinkRef | undefined {
  scanner.skipSpace()
  const { text } = scanner
  let at = scanner.index
  let startHead: string | undefined
  const first = text[at] ?? ''
  if ('<ox'.includes(first) && first && (text[at + 1] === '-' || text[at + 1] === '=')) {
    startHead = first
    at++
  }

  let link: LinkRef
  const end = linkBody(text, at)
  if (end > 0) {
    link = shapeLink(startHead, text.slice(at, end), '')
    scanner.index = end
  }
  else {
    const labelled = labelledLink(text, at)
    if (!labelled)
      return undefined
    link = shapeLink(startHead, labelled.body, cleanLabel(labelled.label))
    scanner.index = labelled.end
  }

  scanner.skipSpace()
  if (scanner.startsWith('|')) {
    scanner.index++
    link.label = cleanLabel(scanner.until('|'))
  }

  return link
}

function shapeLink(startHead: string | undefined, body: string, label: string): LinkRef {
  const last = body.at(-1)!
  const end = HEADS[last] ?? 'none'
  const core = end === 'none' ? body : body.slice(0, -1)
  let line: EdgeLine = 'solid'
  let length = 1
  if (core.startsWith('~')) {
    line = 'invisible'
    length = Math.max(1, core.length - 2)
  }
  else if (core.includes('.')) {
    line = 'dotted'
    length = Math.max(1, core.split('.').length - 1)
  }
  else if (core.startsWith('=')) {
    line = 'thick'
    length = Math.max(1, core.length - (end === 'none' ? 2 : 1))
  }
  else {
    length = Math.max(1, core.length - (end === 'none' ? 2 : 1))
  }

  return {
    line,
    start: startHead ? HEADS[startHead]! : 'none',
    end,
    length: Math.min(length, 4),
    label,
  }
}

function splitStatements(lines: string[]) {
  const result: string[] = []
  for (const line of lines) {
    // Semicolons separate statements outside quoted labels and style values.
    let depth = 0
    let quoted = false
    let start = 0
    for (let index = 0; index < line.length; index++) {
      const char = line[index]
      if (char === '"') {
        quoted = !quoted
      }
      else if (!quoted && '[({'.includes(char!)) {
        depth++
      }
      else if (!quoted && '])}'.includes(char!)) {
        depth = Math.max(0, depth - 1)
      }
      else if (!quoted && depth === 0 && char === ';' && !/^\s*(?:classDef|style|linkStyle)\b/.test(line)) {
        result.push(line.slice(start, index).trim())
        start = index + 1
      }
    }

    result.push(line.slice(start).trim())
  }

  return result.filter(Boolean)
}

function parseFlowchart(source: string): GraphDiagram {
  const { title, header: line, lines: rest } = statements(source)
  // One-line charts put statements after the header: `graph LR; A-->B; B-->C`.
  const separator = line?.indexOf(';') ?? -1
  const header = separator < 0 ? line : line!.slice(0, separator)
  const lines = separator < 0 ? rest : [line!.slice(separator + 1), ...rest]
  const builder = new GraphBuilder()
  const stack: string[] = []
  let subgraphCount = 0

  for (const line of splitStatements(lines)) {
    if (builder.styling(line))
      continue

    const [keyword, rest] = head(line)
    if (keyword === 'subgraph') {
      const spec = rest
      let id: string
      let label: string
      const open = spec.indexOf('[')
      if (open > 0 && spec.endsWith(']') && !/\s/.test(spec.slice(0, open).trim())) {
        id = spec.slice(0, open).trim()
        label = cleanLabel(spec.slice(open + 1, -1))
      }
      else if (spec.startsWith('"') || !spec) {
        label = cleanLabel(spec)
        id = `subgraph-${++subgraphCount}`
      }
      else {
        label = cleanLabel(spec)
        id = /\s/.test(spec) ? `subgraph-${++subgraphCount}` : spec
      }

      const parent = stack.at(-1)
      const cluster: GraphCluster = { id, label }
      if (parent)
        cluster.parent = parent
      builder.clusters.set(id, cluster)
      builder.nodes.delete(id)
      stack.push(id)
      continue
    }

    if (line === 'end') {
      stack.pop()
      continue
    }

    if (keyword === 'direction')
      continue

    if (keyword === 'click') {
      const [id, target] = head(rest)
      const [first, second] = head(target)
      const url = first === 'href' ? head(second)[0] : first
      const link = safeLink(url)
      if (link)
        builder.node(id, stack.at(-1)).link = link
      continue
    }

    parseChain(line, builder, stack.at(-1))
  }

  const dir = direction(/^\S+\s+(\w+)/.exec(header ?? '')?.[1])
  return builder.build(title, dir)
}

function parseChain(line: string, builder: GraphBuilder, parent: string | undefined) {
  const scanner = new Scanner(line)
  let previous = readGroup(scanner, builder, parent)
  if (!previous.length)
    throw new DiagramError(`Unexpected “${line}”.`)

  while (true) {
    scanner.skipSpace()
    if (scanner.done)
      return
    const link = readLink(scanner)
    if (!link)
      throw new DiagramError(`Unexpected “${scanner.text.slice(scanner.index)}”.`)
    const next = readGroup(scanner, builder, parent)
    if (!next.length)
      throw new DiagramError(`A link needs a target in “${line}”.`)
    for (const from of previous) {
      for (const to of next) {
        builder.edge({
          from,
          to,
          label: link.label,
          line: link.line,
          start: link.start,
          end: link.end,
          length: link.length,
        })
      }
    }

    previous = next
  }
}

function readGroup(scanner: Scanner, builder: GraphBuilder, parent: string | undefined) {
  const ids: string[] = []
  while (true) {
    const ref = readNode(scanner)
    if (!ref)
      return ids
    if (builder.clusters.has(ref.id)) {
      ids.push(ref.id)
    }
    else {
      builder.node(ref.id, parent, ref.shape, ref.label)
      for (const name of ref.classes)
        builder.addClass(ref.id, name)
      ids.push(ref.id)
    }

    scanner.skipSpace()
    if (!scanner.startsWith('&'))
      return ids
    scanner.index++
  }
}

function parseState(source: string): GraphDiagram {
  const { title, lines } = statements(source)
  const builder = new GraphBuilder()
  const stack: string[] = []
  let dir: Direction = 'TB'
  let notes = 0
  const descriptions = new Map<string, string[]>()

  const scopeKey = () => stack.at(-1) ?? 'root'
  const endpoint = (raw: string, side: 'from' | 'to') => {
    const name = raw.trim()
    if (name === '[*]') {
      const id = side === 'from' ? `[*]start:${scopeKey()}` : `[*]end:${scopeKey()}`
      builder.node(id, stack.at(-1), side === 'from' ? 'start' : 'end', '')
      return id
    }

    const [id, className] = name.split(':::')
    if (!builder.clusters.has(id!)) {
      builder.node(id!, stack.at(-1), builder.nodes.get(id!)?.shape ?? 'round')
      if (className)
        builder.addClass(id!, className)
    }

    return id!
  }

  for (let index = 0; index < lines.length; index++) {
    const line = lines[index]!.replace(/;$/, '')
    if (builder.styling(line))
      continue

    const [keyword, rest] = head(line)
    if (keyword === 'direction') {
      if (!stack.length)
        dir = direction(rest)
      continue
    }

    if (line === '}') {
      stack.pop()
      continue
    }

    if (line === '--' || line === 'end note')
      continue

    if (keyword.toLowerCase() === 'note') {
      const [where, inline] = colon(rest)
      const words = where.split(/\s+/)
      const side = words[0]!.toLowerCase()
      const target = side === 'over' ? words[1] : words[2]
      if (!target || (side !== 'over' && words[1]?.toLowerCase() !== 'of'))
        throw new DiagramError(`Unexpected “${line}”.`)
      let text = inline
      if (text === undefined) {
        const body: string[] = []
        while (++index < lines.length && lines[index] !== 'end note')
          body.push(lines[index]!)
        text = body.join('\n')
      }

      const id = `note-${++notes}`
      builder.node(id, stack.at(-1), 'note', cleanLabel(text))
      const attached = endpoint(target, 'to')
      const left = side === 'left'
      builder.edge({
        from: left ? id : attached,
        to: left ? attached : id,
        label: '',
        line: 'dotted',
        start: 'none',
        end: 'none',
        length: 1,
      })
      continue
    }

    if (keyword === 'state') {
      let spec = rest
      let label: string | undefined
      if (spec.startsWith('"')) {
        const close = spec.indexOf('"', 1)
        if (close < 0)
          throw new DiagramError(`Unexpected “${line}”.`)
        label = cleanLabel(spec.slice(1, close))
        const [as, after] = head(spec.slice(close + 1).trim())
        if (as !== 'as') {
          // `state "Name"` declares a state whose identifier is its name.
          builder.node(label, stack.at(-1), 'round', label)
          continue
        }

        spec = after
      }

      const composite = spec.endsWith('{')
      if (composite)
        spec = spec.slice(0, -1).trim()
      const [id, marker] = head(spec)
      if (!id)
        throw new DiagramError(`Unexpected “${line}”.`)
      if (composite) {
        const cluster: GraphCluster = { id, label: label ?? id }
        if (stack.length)
          cluster.parent = stack.at(-1)
        builder.clusters.set(id, cluster)
        builder.nodes.delete(id)
        stack.push(id)
      }
      else {
        const special = marker === '<<choice>>' ? 'choice' : marker === '<<fork>>' || marker === '<<join>>' ? 'fork' : undefined
        builder.node(id, stack.at(-1), special ?? 'round', special ? '' : (label ?? id))
      }

      continue
    }

    const arrow = line.indexOf('-->')
    if (arrow > 0) {
      const [target, text] = colon(line.slice(arrow + 3))
      const source = line.slice(0, arrow).trim()
      if (!source || !target || /\s/.test(source) || /\s/.test(target))
        throw new DiagramError(`Unexpected “${line}”.`)
      builder.edge({
        from: endpoint(source, 'from'),
        to: endpoint(target, 'to'),
        label: text ? cleanLabel(text) : '',
        line: 'solid',
        start: 'none',
        end: 'arrow',
        length: 1,
      })
      continue
    }

    const [name, description] = colon(line)
    if (name && !/\s/.test(name) && (description === undefined || description)) {
      const id = endpoint(name, 'to')
      if (description)
        descriptions.set(id, [...(descriptions.get(id) ?? []), cleanLabel(description)])
      continue
    }

    throw new DiagramError(`Unexpected “${line}”.`)
  }

  for (const [id, lines] of descriptions) {
    const node = builder.nodes.get(id)
    if (node)
      node.label = lines.join('\n')
  }

  return builder.build(title, dir)
}

// Longest first at any position. Arrows ending in `x` are tried last: names may contain `-x`.
const ARROWS = ['<<-->>', '<<->>', '-->>', '->>', '--)', '-)', '-->', '->']
const CROSSES = ['--x', '-x']

/** The first arrow before any message text: its index and token. */
function findArrow(line: string) {
  const before = line.includes(':') ? line.slice(0, line.indexOf(':')) : line
  for (const tokens of [ARROWS, CROSSES]) {
    for (let index = 1; index < before.length; index++) {
      const token = tokens.find(candidate => before.startsWith(candidate, index))
      if (token)
        return { index, token }
    }
  }

  return undefined
}

function parseSequence(source: string): SequenceDiagram {
  const { title: frontTitle, lines } = statements(source)
  let title = frontTitle
  const participants = new Map<string, Participant>()
  const items: SequenceItem[] = []
  let autonumber: number | undefined
  // Boxes group participant columns; they open and close like blocks but draw nothing here.
  const open: ('block' | 'box')[] = []

  const participant = (raw: string) => {
    const id = raw.trim()
    let existing = participants.get(id)
    if (!existing) {
      if (participants.size >= DIAGRAM_LIMITS.participants)
        throw new DiagramError('This diagram has too many participants to draw.')
      existing = { id, label: cleanLabel(id), actor: false }
      participants.set(id, existing)
    }

    return existing.id
  }

  const push = (item: SequenceItem) => {
    if (items.length >= DIAGRAM_LIMITS.items)
      throw new DiagramError('This diagram has too many steps to draw.')
    items.push(item)
  }

  for (const raw of lines) {
    const line = raw.replace(/;$/, '')
    const [keyword, rest] = head(line)
    const lower = keyword.toLowerCase()
    if (lower === 'title' || lower.startsWith('title:')) {
      title = cleanLabel(line.slice(5).trim().replace(/^:/, ''))
      continue
    }

    if (keyword === 'autonumber') {
      autonumber = rest === 'off' ? undefined : (Number.parseInt(rest, 10) || 1)
      continue
    }

    const created = keyword === 'create' ? head(rest) : undefined
    const declaration = created ?? [keyword, rest]
    if (declaration[0] === 'participant' || declaration[0] === 'actor') {
      const spec = declaration[1]
      const as = spec.indexOf(' as ')
      const raw = as < 0 ? spec : spec.slice(0, as)
      const properties = raw.indexOf('@{')
      const id = (properties < 0 ? raw : raw.slice(0, properties)).trim()
      const label = as < 0 ? cleanLabel(id) : cleanLabel(spec.slice(as + 4))
      const actor = declaration[0] === 'actor' || (properties >= 0 && raw.slice(properties).replace(/\s/g, '').includes('"type":"actor"'))
      participant(id)
      const known = participants.get(id)!
      known.label = label
      known.actor = actor
      continue
    }

    if (['destroy', 'link', 'links', 'properties', 'details', 'accTitle', 'accTitle:', 'accDescr', 'accDescr:'].includes(keyword))
      continue

    if ((keyword === 'activate' || keyword === 'deactivate') && rest) {
      push({ type: keyword, participant: participant(rest) })
      continue
    }

    if (lower === 'note') {
      const [where, text] = colon(rest)
      const [side, targets] = head(where)
      const placement = side.toLowerCase()
      const list = placement === 'over' ? targets : head(targets)[0].toLowerCase() === 'of' ? head(targets)[1] : ''
      if ((placement === 'left' || placement === 'right' || placement === 'over') && list && text !== undefined) {
        const ids = list.split(',').map(participant)
        push({
          type: 'note',
          placement,
          participants: ids.slice(0, 2),
          label: cleanLabel(text),
        })
        continue
      }
    }

    if (keyword === 'box') {
      open.push('box')
      continue
    }

    if (['loop', 'alt', 'opt', 'par', 'par_over', 'critical', 'break', 'rect'].includes(keyword)) {
      const kind = (keyword === 'par_over' ? 'par' : keyword) as BlockKind
      const block: SequenceItem = { type: 'block', kind, label: kind === 'rect' ? '' : cleanLabel(rest) }
      if (kind === 'rect' && COLOR.test(rest) && rest !== 'transparent')
        block.color = rest
      open.push('block')
      push(block)
      continue
    }

    if ((keyword === 'else' || keyword === 'and' || keyword === 'option') && open.includes('block')) {
      push({ type: 'branch', kind: keyword, label: cleanLabel(rest) })
      continue
    }

    if (line === 'end') {
      if (open.pop() === 'block')
        push({ type: 'end' })
      continue
    }

    const arrow = findArrow(line)
    if (arrow) {
      const { index, token } = arrow
      let [target, text] = colon(line.slice(index + token.length))
      const mark = target[0] === '+' || target[0] === '-' ? target[0] : ''
      target = target.slice(mark.length).trim()
      const source = line.slice(0, index).trim()
      if (!source || !target)
        throw new DiagramError(`Unexpected “${line}”.`)
      const dotted = token.startsWith('--') || token.startsWith('<<--')
      const head: MessageHead = token.endsWith('>>')
        ? 'arrow'
        : token.endsWith('x')
          ? 'cross'
          : token.endsWith(')') ? 'async' : 'none'
      const item: SequenceItem = {
        type: 'message',
        from: participant(source),
        to: participant(target),
        label: cleanLabel(text ?? ''),
        line: dotted ? 'dotted' : 'solid',
        head,
        both: token.startsWith('<<'),
        activate: mark === '+',
        deactivate: mark === '-',
      }
      if (autonumber !== undefined)
        item.number = autonumber++
      push(item)
      continue
    }

    throw new DiagramError(`Unexpected “${line}”.`)
  }

  for (const kind of open) {
    if (kind === 'block')
      items.push({ type: 'end' })
  }

  const diagram: SequenceDiagram = { kind: 'sequence', participants: [...participants.values()], items }
  if (title)
    diagram.title = title
  return diagram
}

function parsePie(source: string): PieDiagram {
  const { title: frontTitle, header, lines } = statements(source)
  let title = frontTitle
  const [, options] = head(header ?? '')
  let showData = options.split(/\s+/).includes('showData')
  const inline = options.indexOf('title ')
  if (inline >= 0)
    title = cleanLabel(options.slice(inline + 6))
  const slices: PieDiagram['slices'] = []
  for (const line of lines) {
    const [keyword, rest] = head(line)
    if (keyword === 'title') {
      title = cleanLabel(rest)
      continue
    }

    if (line === 'showData') {
      showData = true
      continue
    }

    const separator = line.lastIndexOf(':')
    const value = separator > 0 ? line.slice(separator + 1).trim() : ''
    if (/^-?[\d.]+(?:e[+-]?\d+)?$/i.test(value)) {
      const number = Number(value)
      if (!Number.isFinite(number) || number < 0)
        throw new DiagramError('Pie values must be positive numbers.')
      if (slices.length >= DIAGRAM_LIMITS.slices)
        throw new DiagramError('This chart has too many slices to draw.')
      slices.push({ label: cleanLabel(line.slice(0, separator)), value: number })
      continue
    }

    if (/^(?:accTitle|accDescr)\b/.test(line))
      continue
    throw new DiagramError(`Unexpected “${line}”.`)
  }

  if (!slices.some(slice => slice.value > 0))
    throw new DiagramError('This chart has no values to draw.')
  const diagram: PieDiagram = { kind: 'pie', showData, slices }
  if (title)
    diagram.title = title
  return diagram
}
