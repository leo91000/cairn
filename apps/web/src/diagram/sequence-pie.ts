import type {
  BlockKind,
  MessageHead,
  Participant,
  PieDiagram,
  SequenceDiagram,
  SequenceItem,
} from './types'
import {
  cleanLabel,
  colon,
  COLOR,
  head,
  statements,
} from './common'
import { DIAGRAM_LIMITS, DiagramError } from './types'

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

/** Numbering uses hundredths to avoid accumulating floating-point rounding errors. */
function sequenceNumber(value: string) {
  const sign = value.startsWith('-') ? -1 : 1
  const [whole, fraction = ''] = value.replace(/^[+-]/, '').split('.')
  const cents = sign * Number((whole || '0') + fraction.padEnd(2, '0'))
  if (!Number.isSafeInteger(cents))
    throw new DiagramError('The sequence number is too large.')
  return cents
}

/** Sequence semicolons separate steps; entity terminators stay in their labels. */
function sequenceStatements(lines: string[]) {
  const result: string[] = []
  for (const line of lines) {
    let start = 0
    for (const match of line.matchAll(/(?:&?#|&)\w+;|;/g)) {
      if (match[0] !== ';')
        continue
      const part = line.slice(start, match.index).trim()
      if (part)
        result.push(part)
      start = match.index + 1
    }

    const last = line.slice(start).trim()
    if (last)
      result.push(last)
  }

  return result
}

export function parseSequence(source: string): SequenceDiagram {
  const { title: frontTitle, header, lines } = statements(source)
  const inline = header!.slice('sequenceDiagram'.length).trim()
  if (inline && !inline.startsWith(';'))
    throw new DiagramError('The sequence header must end before its first step.')
  let title = frontTitle
  const participants = new Map<string, Participant>()
  const items: SequenceItem[] = []
  let autonumber: number | undefined
  let increment = 100
  // Boxes group participant columns; they open and close like blocks but draw nothing here.
  const open: 'block'[] = []

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

  for (const line of sequenceStatements([inline, ...lines])) {
    const [keyword, rest] = head(line)
    const lower = keyword.toLowerCase()
    if (lower === 'title' || lower.startsWith('title:')) {
      title = cleanLabel(line.slice(5).trim().replace(/^:/, ''))
      continue
    }

    if (keyword === 'autonumber') {
      if (rest === 'off') {
        autonumber = undefined
      }
      else {
        const values = rest ? rest.split(/\s+/) : ['1']
        if (values.length > 2 || values.some(value => !/^[+-]?(?:\d+(?:\.\d{1,2})?|\.\d{1,2})$/.test(value)))
          throw new DiagramError('Sequence numbering supports a start and increment with at most two decimal places.')
        autonumber = sequenceNumber(values[0]!)
        increment = sequenceNumber(values[1] ?? '1')
      }

      continue
    }

    if (keyword === 'create' || keyword === 'destroy')
      throw new DiagramError('Participant creation and destruction are not supported.')

    if (keyword === 'participant' || keyword === 'actor') {
      const spec = rest
      const as = spec.indexOf(' as ')
      const raw = as < 0 ? spec : spec.slice(0, as)
      const properties = raw.indexOf('@{')
      const id = (properties < 0 ? raw : raw.slice(0, properties)).trim()
      const label = as < 0 ? cleanLabel(id) : cleanLabel(spec.slice(as + 4))
      const actor = keyword === 'actor' || (properties >= 0 && raw.slice(properties).replace(/\s/g, '').includes('"type":"actor"'))
      participant(id)
      const known = participants.get(id)!
      known.label = label
      known.actor = actor
      continue
    }

    if (['link', 'links', 'properties', 'details', 'accTitle', 'accTitle:', 'accDescr', 'accDescr:'].includes(keyword))
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

    if (keyword === 'box')
      throw new DiagramError('Sequence participant groups are not supported.')

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
      if (autonumber !== undefined) {
        item.number = autonumber / 100
        autonumber += increment
        if (!Number.isSafeInteger(autonumber))
          throw new DiagramError('The sequence number is too large.')
      }

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

export function parsePie(source: string): PieDiagram {
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
