export interface TablePreview {
  rows: string[][]
  columns: number
  truncated: boolean
}

/** Parses RFC 4180 style CSV/TSV, bounding the rendered table to 200 rows and 50 columns. */
export function parseDelimited(source: string, delimiter: string, limit = 200): TablePreview {
  const rows: string[][] = []
  let row: string[] = []
  let cell = ''
  let quoted = false
  let rowStarted = false
  let rowsTruncated = false
  let columnsTruncated = false
  const rowLimit = Math.max(1, Math.min(limit, 200))
  const pushCell = () => {
    if (row.length < 50)
      row.push(cell)
    else
      columnsTruncated = true
    cell = ''
  }

  const text = source.replace(/^\uFEFF/, '')

  for (let index = 0; index < text.length; index++) {
    const char = text[index]!
    if (quoted || (char !== '\n' && char !== '\r'))
      rowStarted = true
    if (quoted) {
      if (char === '"' && text[index + 1] === '"') {
        if (row.length < 50)
          cell += '"'
        index++
      }
      else if (char === '"') {
        quoted = false
      }
      else {
        if (row.length < 50)
          cell += char
      }

      continue
    }

    if (char === '"' && cell === '') {
      quoted = true
    }
    else if (char === delimiter) {
      pushCell()
    }
    else if (char === '\n' || char === '\r') {
      if (char === '\r' && text[index + 1] === '\n')
        index++
      pushCell()
      rows.push(row)
      rowStarted = false
      row = []
      cell = ''
      if (rows.length >= rowLimit) {
        rowsTruncated = index + 1 < text.length
        break
      }
    }
    else {
      if (row.length < 50)
        cell += char
    }
  }

  if (!rowsTruncated && (rowStarted || cell !== '' || row.length > 0)) {
    pushCell()
    rows.push(row)
  }

  const columns = Math.max(0, ...rows.map(item => item.length))
  return { rows, columns, truncated: rowsTruncated || columnsTruncated }
}

/** HTML rendered inside a script-less sandbox, without network access. */
export function inertDocument(html: string) {
  const policy = '<meta http-equiv="Content-Security-Policy" content="default-src \'none\'; img-src data:; media-src data:; style-src \'unsafe-inline\'; font-src data:">'
  const doctype = /^\s*<!doctype[^>]*>/i.exec(html)?.[0] ?? ''
  return `${doctype}${policy}${html.slice(doctype.length)}`
}

/** Adds whitespace without rewriting JSON numbers, string escapes or duplicate keys. */
export function formatJson(source: string): string {
  try {
    JSON.parse(source)
  }
  catch {
    return source
  }

  const tokens = source.match(/"(?:\\.|[^"\\])*"|[{}[\],:]|[^\s{}[\],:]+/g) ?? []
  const output: string[] = []
  const outputBudget = Math.min(1024 * 1024, Math.max(1024, source.length * 4))
  let outputLength = 0
  const append = (text: string) => {
    outputLength += text.length
    if (outputLength <= outputBudget)
      output.push(text)
  }

  let depth = 0
  const newline = () => append(`\n${'  '.repeat(depth)}`)
  for (let index = 0; index < tokens.length; index++) {
    const token = tokens[index]!
    if (token === '{' || token === '[') {
      append(token)
      if (tokens[index + 1] === (token === '{' ? '}' : ']')) {
        append(tokens[++index]!)
        if (outputLength > outputBudget)
          return source
        continue
      }

      depth++
      if (depth > 100)
        return source
      newline()
    }
    else if (token === '}' || token === ']') {
      depth--
      newline()
      append(token)
    }
    else if (token === ',') {
      append(token)
      newline()
    }
    else if (token === ':') {
      append(': ')
    }
    else {
      append(token)
    }

    if (outputLength > outputBudget)
      return source
  }

  return output.join('')
}
