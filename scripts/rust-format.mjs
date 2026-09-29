import { spawnSync } from 'node:child_process'
import process from 'node:process'
import { fileURLToPath } from 'node:url'

const check = process.argv[2] === '--check'

if (process.argv.length > 3 || (process.argv[2] && !check)) {
  console.error('Usage: node scripts/rust-format.mjs [--check]')
  process.exit(1)
}

const cwd = fileURLToPath(new URL('..', import.meta.url))
const commands = [
  ['cargo', ['fmt', '--all', ...(check ? ['--check'] : [])]],
  [process.execPath, ['scripts/rust-spacing.mjs', ...(check ? ['--check'] : [])]],
]

for (const [command, args] of commands) {
  const result = spawnSync(command, args, { cwd, stdio: 'inherit' })

  if (result.error)
    console.error(result.error.message)

  if (result.status !== 0)
    process.exit(result.status ?? 1)
}
