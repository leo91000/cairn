import type { Buffer } from 'node:buffer'
import type { ChildProcess } from 'node:child_process'
import type { Config } from './config.ts'
import { execFile, spawn } from 'node:child_process'
import path from 'node:path'
import process from 'node:process'
import { promisify } from 'node:util'
import { AppError } from './errors.ts'

const exec = promisify(execFile)
export interface DeviceFlow {
  provider: 'codex' | 'github'
  state: 'pending' | 'complete' | 'failed'
  url: string
  code: string
  error?: string
}
export function deviceDetails(text: string) {
  return {
    code: text.match(/\b[A-Z0-9]{4}-[A-Z0-9]{4,5}\b/)?.[0] ?? '',
    url:
      text.match(/https:\/\/(?:auth\.openai\.com|github\.com)\/[\w/-]+/)?.[0]
      ?? '',
  }
}

export class Connections {
  cache: { at: number, value: unknown } | undefined
  flow: DeviceFlow | undefined
  child: ChildProcess | undefined
  constructor(public config: Config) {}
  env() {
    const env: NodeJS.ProcessEnv = {
      ...process.env,
      HOME: this.config.home,
      CODEX_HOME: path.join(this.config.home, '.codex'),
    }
    delete env.CODEX_API_KEY
    delete env.OPENAI_API_KEY
    return env
  }

  async status(force = false) {
    if (!force && this.cache && Date.now() - this.cache.at < 30000)
      return this.cache.value
    const check = async (provider: 'codex' | 'github') => {
      const binary
        = provider === 'codex' ? this.config.codexBin : this.config.ghBin
      try {
        const version = await exec(binary, ['--version'], {
          env: this.env(),
          timeout: 5000,
          maxBuffer: 10000,
        })
        const login = await exec(
          binary,
          provider === 'codex'
            ? ['login', 'status']
            : ['api', 'user', '--jq', '.login'],
          { env: this.env(), timeout: 10000, maxBuffer: 10000 },
        ).catch(() => null)
        const auth
          = provider === 'codex'
            ? `${login?.stdout ?? ''}${login?.stderr ?? ''}`
            : login?.stdout?.trim()
        return {
          provider,
          installed: true,
          version: version.stdout.split('\n')[0],
          connected:
            provider === 'codex' ? !!auth?.includes('ChatGPT') : !!login,
          account:
            provider === 'codex'
              ? auth?.includes('ChatGPT')
                ? 'ChatGPT subscription'
                : 'Not signed in'
              : auth || 'Not signed in',
        }
      }
      catch {
        return {
          provider,
          installed: false,
          connected: false,
          account: 'CLI not installed',
          version: '',
        }
      }
    }

    const value = await Promise.all([check('codex'), check('github')])
    this.cache = { at: Date.now(), value }
    return value
  }

  start(provider: 'codex' | 'github') {
    if (this.child)
      throw new AppError(409, 'A sign-in is already in progress.')
    const flow: DeviceFlow = {
      provider,
      state: 'pending',
      url: '',
      code: '',
    }
    this.flow = flow
    const child = spawn(
      provider === 'codex' ? this.config.codexBin : this.config.ghBin,
      provider === 'codex'
        ? ['login', '--device-auth']
        : [
            'auth',
            'login',
            '--hostname',
            'github.com',
            '--git-protocol',
            'https',
            '--web',
          ],
      { env: this.env(), stdio: ['pipe', 'pipe', 'pipe'] },
    )
    this.child = child
    let buffer = ''
    const receive = (chunk: Buffer) => {
      buffer = (buffer + chunk.toString()).slice(-16000)
      const details = deviceDetails(buffer)
      if (this.flow === flow) {
        if (details.code)
          this.flow.code = details.code
        if (details.url)
          this.flow.url = details.url
      }
    }

    child.stdout?.on('data', receive)
    child.stderr?.on('data', receive)
    child.stdin?.on('error', () => {})
    child.stdin?.end('\n')
    const timeout = setTimeout(() => child.kill('SIGTERM'), 15 * 60000)
    timeout.unref()
    const finish = (success: boolean) => {
      clearTimeout(timeout)
      if (this.child !== child)
        return
      if (this.flow === flow) {
        this.flow.state = success ? 'complete' : 'failed'
        if (!success) {
          this.flow.error
            = 'Sign-in did not complete. Retry or use the documented container login command.'
        }
      }

      this.child = undefined
      this.cache = undefined
    }

    child.once('error', () => finish(false))
    child.once('close', code => finish(code === 0))
    return this.flow
  }

  cancel() {
    this.child?.kill('SIGTERM')
    this.child = undefined
    this.flow = undefined
  }
}
