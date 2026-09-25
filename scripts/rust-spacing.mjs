import { execFileSync } from 'node:child_process'
import { readFileSync, writeFileSync } from 'node:fs'
import process from 'node:process'
import rust from '@ast-grep/lang-rust'
import { parse, registerDynamicLanguage } from '@ast-grep/napi'

registerDynamicLanguage({ rust })

const attachments = new Set(['attribute_item', 'line_comment', 'block_comment'])

function compactGroup(node) {
  const kind = node.kind()

  if (kind === 'use_declaration')
    return 'imports'

  if (node.range().start.line !== node.range().end.line)
    return null

  if (kind === 'mod_item' && !node.children().some(child => child.kind() === 'declaration_list'))
    return 'modules'

  if (['const_item', 'static_item'].includes(kind))
    return 'constants'

  return null
}

// Only insert whitespace between declarations. Never interpret strings, macro
// token trees, function statements, or the contents of comments as declarations.
export function spaceRustDeclarations(source) {
  const root = parse('rust', source).root()
  const insertions = new Map()
  const scopes = [root, ...root.findAll({ rule: { kind: 'declaration_list' } })]

  for (const scope of scopes) {
    let previous
    let leading

    for (const node of scope.children()) {
      const kind = node.kind()

      if (['{', '}', ';'].includes(kind))
        continue

      if (attachments.has(kind)) {
        // A comment on the previous declaration's line belongs to that line.
        if (!previous || node.range().start.line > previous.range().end.line)
          leading ??= node
        continue
      }

      const start = (leading ?? node).range().start.index

      if (previous && (!compactGroup(node) || compactGroup(node) !== compactGroup(previous))) {
        const gap = source.slice(previous.range().end.index, start)

        if (!/\n[\t \r]*\n/.test(gap)) {
          const lineStart = source.lastIndexOf('\n', start - 1) + 1
          const onOwnLine = /^[\t ]*$/.test(source.slice(lineStart, start))
          insertions.set(onOwnLine ? lineStart : start, onOwnLine ? '\n' : '\n\n')
        }
      }

      previous = node
      leading = undefined
    }
  }

  let formatted = source

  for (const [index, whitespace] of [...insertions].sort(([a], [b]) => b - a))
    formatted = formatted.slice(0, index) + whitespace + formatted.slice(index)

  return formatted
}

if (import.meta.main) {
  const check = process.argv[2] === '--check'

  if (process.argv.length > 3 || (process.argv[2] && !check)) {
    console.error('Usage: node scripts/rust-spacing.mjs [--check]')
    process.exit(1)
  }

  const files = execFileSync('git', ['ls-files', '--cached', '--others', '--exclude-standard', '-z', '--', '*.rs'], { encoding: 'utf8' }).split('\0').filter(Boolean)
  let changed = false

  for (const file of new Set(files)) {
    let source

    try {
      source = readFileSync(file, 'utf8')
    }
    catch (error) {
      if (error.code === 'ENOENT')
        continue
      throw error
    }

    const formatted = spaceRustDeclarations(source)

    if (formatted === source)
      continue

    changed = true

    if (check)
      console.error(`${file}: separate declarations with a blank line (pnpm format:rust)`)
    else
      writeFileSync(file, formatted)
  }

  if (check && changed)
    process.exit(1)
}
