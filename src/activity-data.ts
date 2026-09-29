export type DataValue = null | boolean | number | string | DataValue[] | { [key: string]: DataValue }
export type ContentPart = { kind: 'text', text: string } | { kind: 'data', value: DataValue, source: string } | { kind: 'incomplete', source: string }

function jsonEnd(source: string, start: number) {
  const stack: string[] = []
  let quoted = false
  let escaped = false
  for (let index = start; index < source.length; index++) {
    const char = source[index]
    if (quoted) {
      if (escaped)
        escaped = false
      else if (char === '\\')
        escaped = true
      else if (char === '"')
        quoted = false
      continue
    }

    if (char === '"') {
      quoted = true
    }
    else if (char === '{' || char === '[') {
      stack.push(char)
    }
    else if (char === '}' || char === ']') {
      if (stack.pop() !== (char === '}' ? '{' : '['))
        return -2
      if (!stack.length)
        return index + 1
    }
  }

  return -1
}

function incompleteJSON(source: string) {
  const value = source.trimStart()
  return /^[[{]\s*["[{]/.test(value) && jsonEnd(value, 0) === -1
}

/** Preview complete JSON and identify incomplete output without inventing missing data. */
export function contentParts(source: string): ContentPart[] {
  if (source.length > 500000)
    return [{ kind: 'text', text: source }]
  const parts: ContentPart[] = []
  let cursor = 0
  let index = 0
  while (index < source.length && parts.length < 100) {
    if (source[index] === '`' || source.startsWith('~~~', index)) {
      const marker = source.slice(index).match(/^(`+|~{3,})/)![0]
      const end = source.indexOf(marker, index + marker.length)
      if (end < 0) {
        const unfinished = source.slice(index + marker.length).match(/^json[ \t]*\r?\n([\s\S]*)$/i)
        if (marker.length >= 3 && unfinished && incompleteJSON(unfinished[1])) {
          if (index > cursor)
            parts.push({ kind: 'text', text: source.slice(cursor, index) })
          parts.push({ kind: 'incomplete', source: unfinished[1] })
          cursor = source.length
        }

        break
      }

      const fenced = source.slice(index + marker.length, end)
      const match = marker.length >= 3 && fenced.match(/^(?:json)?[ \t]*\n([\s\S]*)$/i)
      if (match) {
        try {
          const value = JSON.parse(match[1])
          if (value !== null && typeof value === 'object') {
            if (index > cursor)
              parts.push({ kind: 'text', text: source.slice(cursor, index) })
            parts.push({ kind: 'data', value, source: match[1] })
            cursor = end + marker.length
          }
        }
        catch {
          if (incompleteJSON(match[1])) {
            if (index > cursor)
              parts.push({ kind: 'text', text: source.slice(cursor, index) })
            parts.push({ kind: 'incomplete', source: match[1] })
            cursor = end + marker.length
          }
        }
      }

      index = end + marker.length
      continue
    }

    if ((source[index] === '{' || source[index] === '[') && (index === 0 || /[\s:]/.test(source[index - 1]))) {
      const end = jsonEnd(source, index)
      if (end > index) {
        try {
          const value = JSON.parse(source.slice(index, end))
          // Preserve Markdown links, whose link text can happen to be valid JSON.
          if (source[end] !== '(' && source[end] !== '[') {
            if (index > cursor)
              parts.push({ kind: 'text', text: source.slice(cursor, index) })
            parts.push({ kind: 'data', value, source: source.slice(index, end) })
            cursor = end
            index = end
            continue
          }
        }
        catch { /* Ordinary prose may contain braces. */ }

        index = end
        continue
      }

      // A streaming/truncated object should never expose a nested fragment as complete.
      if (end < 0 && /^[[{]\s*["[{]/.test(source.slice(index))) {
        if (end === -1) {
          if (index > cursor)
            parts.push({ kind: 'text', text: source.slice(cursor, index) })
          parts.push({ kind: 'incomplete', source: source.slice(index) })
          cursor = source.length
        }

        break
      }
    }

    index++
  }

  if (cursor < source.length)
    parts.push({ kind: 'text', text: source.slice(cursor) })
  return parts.length ? parts : [{ kind: 'text', text: source }]
}

/** Describe saved results without putting serialized data in the collapsed card. */
export function outputSummary(source: string) {
  const parts = contentParts(source)
  const results = parts.filter(part => part.kind === 'data')
  const incomplete = parts.some(part => part.kind === 'incomplete')
  if (results.length === 1)
    return { title: dataTitle(results[0].value), subtitle: `${dataSummary(results[0].value)}${incomplete ? ' · Additional output incomplete' : ''}` }
  if (results.length > 1)
    return { title: 'Saved results', subtitle: `${results.length} results${incomplete ? ' · Additional output incomplete' : ''}` }
  if (incomplete)
    return { title: 'Incomplete result', subtitle: 'The saved JSON ends before the result is complete' }
  return { title: 'Recorded output', subtitle: source.split('\n').find(line => line.trim())?.slice(0, 180) || 'No output recorded' }
}

export function fieldLabel(key: string) {
  return key.replace(/([a-z0-9])([A-Z])/g, '$1 $2').replace(/[_-]/g, ' ').replace(/^./, char => char.toUpperCase())
}

export function dataObject(value: DataValue): value is { [key: string]: DataValue } {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}

export function dataSummary(value: DataValue): string {
  if (Array.isArray(value))
    return `${value.length.toLocaleString()} ${value.length === 1 ? 'item' : 'items'}`
  if (dataObject(value)) {
    const count = Object.keys(value).length
    return `${count} ${count === 1 ? 'field' : 'fields'}`
  }

  if (value === null)
    return 'Not set'
  if (value === '')
    return 'Empty'
  if (typeof value === 'boolean')
    return value ? 'Yes' : 'No'
  return `${value}`
}

export function dataTitle(value: DataValue) {
  if (Array.isArray(value)) {
    if (value.length && value.every(item => dataObject(item) && Array.isArray(item.jobs)))
      return 'Workflow checks'
    return 'Results'
  }

  if (dataObject(value)) {
    if (Array.isArray(value.jobs))
      return 'Workflow checks'
    if (Array.isArray(value.files) && ('headRefOid' in value || 'baseRefOid' in value))
      return 'Pull request details'
  }

  return 'Structured result'
}

export function statusTone(key: string, value: DataValue) {
  if (!['status', 'conclusion', 'state'].includes(key.toLowerCase()) || typeof value !== 'string')
    return ''
  if (['success', 'succeeded', 'passed', 'merged'].includes(value.toLowerCase()))
    return 'success'
  if (['failure', 'failed', 'error', 'timed_out', 'action_required'].includes(value.toLowerCase()))
    return 'error'
  return 'neutral'
}
