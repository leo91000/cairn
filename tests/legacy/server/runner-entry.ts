import { spawn } from 'node:child_process'
import { readFile } from 'node:fs/promises'
import process from 'node:process'
import { runChat } from './chat-process.ts'
import { toolkitEnvironment } from './toolkit.ts'

async function main() {
  const plan = JSON.parse(await readFile('/run/leo-plan.json', 'utf8'))
  if (plan.chat) {
    Object.assign(process.env, await toolkitEnvironment('/home/node'), plan.mcpEnv ?? {})
    await runChat(plan.chat)
    return
  }

  const child = spawn('codex', plan.args, { cwd: plan.cwd, env: { ...await toolkitEnvironment('/home/node'), ...(plan.mcpEnv?.LEO_MCP_RUN_TOKEN ? { LEO_MCP_RUN_TOKEN: plan.mcpEnv.LEO_MCP_RUN_TOKEN } : {}) }, stdio: ['pipe', 'inherit', 'inherit'] })
  child.stdin.on('error', () => {})
  child.stdin.end(plan.prompt)
  child.on('error', (error) => {
    console.error(error)
    process.exitCode = 1
  })
  child.on('close', (code) => {
    process.exitCode = code ?? 1
  })
  process.on('SIGTERM', () => child.kill('SIGTERM'))
}

main().catch((error) => {
  console.error(error)
  process.exitCode = 1
})
