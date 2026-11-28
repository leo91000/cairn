import type {
  DiagramStyle,
  Direction,
  GraphCluster,
  GraphDiagram,
  GraphEdge,
  GraphNode,
  NodeShape,
} from './types'
import { DIAGRAM_LIMITS, DiagramError } from './types'

export interface Statements {
  title?: string
  header?: string
  lines: string[]
}

/** Remove front matter, directives and comments; return the header and following lines. */
export function statements(source: string): Statements {
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

export function unquote(value: string) {
  const trimmed = value.trim()
  if (trimmed.length >= 2 && ((trimmed.startsWith('"') && trimmed.endsWith('"')) || (trimmed.startsWith('\'') && trimmed.endsWith('\''))))
    return trimmed.slice(1, -1)
  return trimmed
}

/** The first word of a statement and the trimmed rest. Plain scanning keeps parsing linear. */
export function head(line: string): [string, string] {
  const space = line.search(/\s/)
  return space < 0 ? [line, ''] : [line.slice(0, space), line.slice(space).trim()]
}

/** Text before and after the first colon, both trimmed; `after` is undefined without one. */
export function colon(text: string): [string, string | undefined] {
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

export const COLOR = /^(?:#[\da-f]{3,8}|(?:rgba?|hsla?)\([\d\s.,%/-]+\)|[a-z]{3,20})$/i

export function parseStyle(value: string): DiagramStyle | undefined {
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
export function splitStyle(value: string) {
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

export function mergeStyle(...styles: (DiagramStyle | undefined)[]) {
  const merged = Object.assign({}, ...styles.filter(Boolean)) as DiagramStyle
  return Object.keys(merged).length ? merged : undefined
}

export function safeLink(value: string) {
  try {
    const url = new URL(unquote(value))
    return url.protocol === 'https:' || url.protocol === 'http:' ? url.href : undefined
  }
  catch {
    return undefined
  }
}

export function direction(value: string | undefined): Direction {
  const upper = value?.toUpperCase()
  if (upper === 'LR' || upper === 'RL' || upper === 'BT')
    return upper
  return 'TB'
}

export class GraphBuilder {
  clusterDirections = new Map<string, Direction>()
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

  build(title: string | undefined, dir: Direction, inheritExternalDirection = false): GraphDiagram {
    // Reusing a cluster ID can reparent an existing ancestor. Reject cycles before layout.
    if (this.clusters.size > DIAGRAM_LIMITS.clusters)
      throw new DiagramError('This diagram has too many subgraphs to draw.')
    const checked = new Map<string, number>()
    for (const id of this.clusters.keys()) {
      const ancestors = new Set<string>()
      let current: string | undefined = id
      while (current && !checked.has(current)) {
        if (ancestors.has(current))
          throw new DiagramError('Subgraphs cannot contain themselves.')
        ancestors.add(current)
        if (ancestors.size > DIAGRAM_LIMITS.clusterDepth)
          throw new DiagramError('This diagram nests subgraphs too deeply to draw.')
        current = this.clusters.get(current)?.parent
      }

      let depth = current ? checked.get(current) ?? 0 : 0
      for (const ancestor of [...ancestors].reverse()) {
        depth++
        if (depth > DIAGRAM_LIMITS.clusterDepth)
          throw new DiagramError('This diagram nests subgraphs too deeply to draw.')
        checked.set(ancestor, depth)
      }
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

    const inGroup = (key: string, group: string) => {
      if (key === group)
        return true
      let parent = this.nodes.get(key)?.parent ?? this.clusters.get(key)?.parent
      while (parent) {
        if (parent === group)
          return true
        parent = this.clusters.get(parent)?.parent
      }

      return false
    }

    for (const [id, requested] of this.clusterDirections) {
      if (requested === dir)
        continue
      const inside = (key: string) => inGroup(key, id)

      // Mermaid inherits the parent direction when a member node has an external link.
      const externalMemberLink = this.edges.some(edge =>
        (this.nodes.has(edge.from) && !this.clusters.has(edge.from) && inside(edge.from) && !inside(edge.to))
        || (this.nodes.has(edge.to) && !this.clusters.has(edge.to) && inside(edge.to) && !inside(edge.from)))
      if (!inheritExternalDirection || !externalMemberLink)
        throw new DiagramError('Subgraphs with a different local direction are not supported.')
    }

    if (this.edges.some(edge => edge.from === edge.to && this.clusters.has(edge.from)))
      throw new DiagramError('Self-transitions on composite states or subgraphs are not supported.')

    if (this.edges.some(edge =>
      (this.clusters.has(edge.from) && inGroup(edge.to, edge.from))
      || (this.clusters.has(edge.to) && inGroup(edge.from, edge.to)))) {
      throw new DiagramError('Links between a group and its own descendants are not supported.')
    }

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
