import { execFileSync, spawnSync } from 'node:child_process'
import process from 'node:process'

const diff = () => execFileSync('git', ['diff', '--no-ext-diff', '--binary'], { maxBuffer: 64 * 1024 * 1024 })

function run(command, args) {
  const result = spawnSync(command, args, { stdio: 'inherit' })

  if (result.error) {
    console.error(result.error.message)
    process.exit(1)
  }

  if (result.status !== 0)
    process.exit(result.status ?? 1)
}

const before = diff()
run('pnpm', ['lint:fix'])

const staged = execFileSync('git', ['diff', '--cached', '--name-only', '-z'], { encoding: 'utf8' }).split('\0')

if (staged.some(file => file.startsWith('android/') && /\.kts?$/.test(file)))
  run('pnpm', ['format:android'])

if (!before.equals(diff())) {
  console.error('Lint fixes changed tracked files. Review and stage the fixes, then retry your commit.')
  process.exit(1)
}

run('cargo', ['check', '--locked', '--workspace', '--all-targets'])
run('cargo', ['clippy', '--locked', '--workspace', '--all-targets', '--', '-D', 'warnings'])
