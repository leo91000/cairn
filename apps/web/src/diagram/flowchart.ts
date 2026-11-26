import type {
  Direction,
  EdgeHead,
  EdgeLine,
  GraphCluster,
  GraphDiagram,
  NodeShape,
} from './types'
import {
  cleanLabel,
  colon,
  direction,
  GraphBuilder,
  head,
  safeLink,
  statements,
} from './common'
import { DiagramError } from './types'

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

function run(text: string, from: number, char: string) {
  let end = from
  while (text[end] === char)
    end++
  return end - from
}

function headEnd(text: string, index: number) {
  if (text[index] === '>')
    return index + 1
  return (text[index] === 'o' || text[index] === 'x') ? index + 1 : -1
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
    if (open === '-.') {
      if (text[index] === '.' && text[index - 1] !== '.') {
        const close = index + run(text, index, '.')
        if (text[close] === '-') {
          end = headEnd(text, close + 1)
          end = end >= 0 ? end : close + 1
        }
      }
    }
    else if (text[index] === open[0] && text[index - 1] !== open[0]) {
      end = linkBody(text, index)
    }

    if (end <= 0)
      continue
    const label = text.slice(at + 2, index).trim()
    if (label) {
      const body = `${open === '-.' ? '-' : ''}${text.slice(index, end)}`
      return { label, body, end }
    }
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
    const parts: string[] = []
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
      else if (!quoted && depth === 0 && char === ';') {
        parts.push(line.slice(start, index).trim())
        start = index + 1
      }
    }

    parts.push(line.slice(start).trim())

    // Preserve semicolon-separated CSS properties, but never absorb the next graph statement.
    let styling = false
    for (const part of parts) {
      if (!part)
        continue
      if (styling && /^[a-z][a-z-]*\s*:/i.test(part)) {
        result[result.length - 1] += `;${part}`
      }
      else {
        result.push(part)
        styling = /^(?:classDef|style|linkStyle)\b/.test(part)
      }
    }
  }

  return result
}

export function parseFlowchart(source: string): GraphDiagram {
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

    if (keyword === 'direction') {
      const parent = stack.at(-1)
      if (!parent)
        throw new DiagramError('Set the flowchart direction in its header.')
      builder.clusterDirections.set(parent, direction(rest))
      continue
    }

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
  return builder.build(title, dir, true)
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

/** Endpoint class suffixes are not the single-colon delimiter before state label text. */
function stateLabel(text: string): [string, string | undefined] {
  for (let index = 0; index < text.length; index++) {
    if (text[index] !== ':')
      continue
    if (text.startsWith(':::', index)) {
      index += 2
      continue
    }

    return [text.slice(0, index).trim(), text.slice(index + 1).trim()]
  }

  return [text.trim(), undefined]
}

export function parseState(source: string): GraphDiagram {
  const { title, lines } = statements(source)
  const builder = new GraphBuilder()
  const stack: string[] = []
  let dir: Direction = 'TB'
  let notes = 0
  const descriptions = new Map<string, string[]>()

  const scopeKey = () => stack.length ? `state:${stack.at(-1)}` : 'document'
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
      else
        builder.clusterDirections.set(stack.at(-1)!, direction(rest))
      continue
    }

    if (line === '}') {
      stack.pop()
      continue
    }

    if (line === '--')
      throw new DiagramError('Concurrent state regions are not supported.')

    if (line === 'end note')
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

      if (side !== 'left' && side !== 'right')
        throw new DiagramError('Only left and right state notes are supported.')
      const id = `\u0000note:${++notes}`
      const note = builder.node(id, stack.at(-1), 'note', cleanLabel(text))
      const attached = endpoint(target, 'to')
      note.annotation = { target: attached, side }
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
      const [target, text] = stateLabel(line.slice(arrow + 3))
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

    const [name, description] = stateLabel(line)
    if (name && !/\s/.test(name) && (description === undefined || description)) {
      const id = endpoint(name, 'to')
      if (description)
        descriptions.set(id, [...(descriptions.get(id) ?? []), cleanLabel(description)])
      continue
    }

    throw new DiagramError(`Unexpected “${line}”.`)
  }

  for (const [id, lines] of descriptions) {
    const target = builder.clusters.get(id) ?? builder.nodes.get(id)
    if (target)
      target.label = lines.join('\n')
  }

  return builder.build(title, dir)
}
