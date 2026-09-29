export interface CommandPresentation {
  kind: 'command' | 'read' | 'search' | 'browse'
  title: string
  subtitle: string
  language: string
  paths: string[]
  command: string
}

/** Tokenize only simple shell commands. Never execute or guess through operators. */
function words(source: string): string[] | undefined {
  const result: string[] = []
  let token = ''
  let quote = ''
  let started = false
  for (let index = 0; index < source.length; index++) {
    const char = source[index]
    if (quote === '\'') {
      if (char === quote)
        quote = ''
      else
        token += char
      continue
    }

    if (char === '\\') {
      const next = source[++index]
      if (!next || next === '\n')
        return
      token += quote === '"' && !['$', '`', '"', '\\', '\n'].includes(next) ? `\\${next}` : next
      started = true
      continue
    }

    if (char === '$' || char === '`')
      return
    if (quote) {
      if (char === quote)
        quote = ''
      else
        token += char
      continue
    }

    if (char === '"' || char === '\'') {
      quote = char
      started = true
      continue
    }

    if (/[;&|<>\n(){}*?[\]]/.test(char) || (char === '#' && !started))
      return
    if (/\s/.test(char)) {
      if (started)
        result.push(token)
      token = ''
      started = false
      continue
    }

    started = true
    token += char
  }

  if (quote)
    return
  if (started)
    result.push(token)
  return result
}

export function fileLanguage(path: string) {
  const extension = path.split('.').at(-1)?.toLowerCase() ?? ''
  const languages: Record<string, string> = {
    ts: 'typescript',
    tsx: 'typescript',
    js: 'javascript',
    mjs: 'javascript',
    cjs: 'javascript',
    jsx: 'javascript',
    json: 'json',
    yaml: 'yaml',
    yml: 'yaml',
    css: 'css',
    vue: 'xml',
    html: 'xml',
    svg: 'xml',
    xml: 'xml',
    rs: 'rust',
    py: 'python',
    sql: 'sql',
    sh: 'bash',
    bash: 'bash',
    zsh: 'bash',
    md: 'markdown',
  }
  return languages[extension] ?? 'plaintext'
}

function commandWords(command: string) {
  let tokens = words(command)
  let source = command
  if (tokens?.length === 3 && /(?:^|\/)(?:ba|z)?sh$/.test(tokens[0]) && ['-c', '-lc', '-cl'].includes(tokens[1])) {
    source = tokens[2]
    tokens = words(source)
  }

  return { tokens, source }
}

/** Exit 1 is a documented result only for these simple, unambiguous commands. */
export function expectedCommandOutcome(command: string, exitCode: number | undefined) {
  if (exitCode !== 1)
    return
  const { tokens } = commandWords(command)
  if (!tokens?.length)
    return
  const [bin, ...args] = tokens
  const name = bin.split('/').at(-1)
  if (name === 'rg' || name === 'grep')
    return 'No matches'
  if (name === 'diff' || (name === 'git' && args[0] === 'diff' && args.some(arg => arg === '--exit-code' || arg === '--quiet')))
    return 'Differences found'
}

export function describeCommand(command: string): CommandPresentation {
  const { tokens, source } = commandWords(command)
  const result: CommandPresentation = {
    kind: 'command',
    title: 'Run command',
    subtitle: source,
    language: 'plaintext',
    paths: [],
    command: source,
  }
  if (!tokens?.length)
    return result
  const [bin, ...args] = tokens
  const name = bin.split('/').at(-1)
  let paths: string[] = []
  if (name === 'cat' && args.length && args.every(arg => arg && (!arg.startsWith('-') || arg === '--')))
    paths = args.filter(arg => arg !== '--')
  if (name === 'sed' && args.length === 3 && args[0] === '-n' && /^\d+(?:,(?:\d+|\$))?p$/.test(args[1]) && !args[2].startsWith('-'))
    paths = [args[2]]
  if (['head', 'tail'].includes(name ?? '') && args.length === 1 && !args[0].startsWith('-'))
    paths = args
  if (['head', 'tail'].includes(name ?? '') && args.length === 3 && args[0] === '-n' && /^\+?\d+$/.test(args[1]) && !args[2].startsWith('-'))
    paths = [args[2]]
  if (paths.length) {
    return {
      ...result,
      kind: 'read',
      title: paths.length === 1 ? `Read ${paths[0].split('/').at(-1)}` : `Read ${paths.length} files`,
      subtitle: paths.join(' · '),
      paths,
      language: paths.length === 1 ? fileLanguage(paths[0]) : 'plaintext',
    }
  }

  if (name === 'rg' || name === 'grep')
    return { ...result, kind: args.includes('--files') ? 'browse' : 'search', title: args.includes('--files') ? 'Browse files' : 'Search files' }
  if (['ls', 'fd', 'find', 'pwd'].includes(name ?? ''))
    return { ...result, kind: 'browse', title: name === 'pwd' ? 'Locate workspace' : 'Browse files' }
  if (['pnpm', 'npm', 'yarn', 'bun', 'cargo'].includes(name ?? '')) {
    const action = args[0] === 'run' ? args[1] : args[0]
    if (action && /^(?:test(?::.*)?|check|typecheck|lint|build)$/.test(action)) {
      result.title = action.startsWith('test')
        ? 'Run tests'
        : {
            check: 'Run checks',
            typecheck: 'Check types',
            lint: 'Lint code',
            build: 'Build project',
          }[action] || 'Run command'
    }
  }

  if (name === 'git') {
    result.title = {
      status: 'Check Git status',
      diff: 'Review changes',
      log: 'Read commit history',
      show: 'Inspect Git object',
      fetch: 'Fetch repository',
      add: 'Stage changes',
      commit: 'Create commit',
      push: 'Push changes',
    }[args[0]] || 'Run Git command'
    if (args[0] === 'diff' && !args.some(arg => /^--(?:stat|name|numstat|shortstat)/.test(arg)))
      result.language = 'diff'
  }

  return result
}
