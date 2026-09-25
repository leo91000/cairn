import os from 'node:os'
import path from 'node:path'
import process from 'node:process'

export interface Config {
  dataDir: string
  home: string
  workspaceRoots: string[]
  publicUrl: string
  host: string
  port: number
  setupToken: string
  codexBin: string
  ghBin: string
  concurrency: number
  logger: boolean
  workerEnabled: boolean
  runnerUrl: string
}
export function config(overrides: Partial<Config> = {}): Config {
  const settings: Config = {
    dataDir: path.resolve(process.env.DATA_DIR || '.data'),
    home: process.env.AGENT_HOME || os.homedir(),
    workspaceRoots: (process.env.WORKSPACE_ROOTS || process.cwd())
      .split(path.delimiter)
      .map(p => path.resolve(p)),
    publicUrl: process.env.PUBLIC_URL || 'http://localhost:4310',
    host: process.env.HOST || '127.0.0.1',
    port: Number(process.env.PORT || 4310),
    setupToken: process.env.SETUP_TOKEN || '',
    codexBin: process.env.CODEX_BIN || 'codex',
    ghBin: process.env.GH_BIN || 'gh',
    concurrency: Number(process.env.CONCURRENCY || 1),
    logger: process.env.NODE_ENV !== 'test',
    workerEnabled: true,
    runnerUrl: process.env.RUNNER_URL || '',
    ...overrides,
  }
  if (
    !Number.isInteger(settings.concurrency)
    || settings.concurrency < 1
  ) {
    throw new Error('CONCURRENCY must be a positive integer.')
  }

  if (
    !Number.isInteger(settings.port)
    || settings.port < 0
    || settings.port > 65535
  ) {
    throw new Error('PORT must be a valid port number.')
  }

  const url = new URL(settings.publicUrl)
  if (
    !['http:', 'https:'].includes(url.protocol)
    || url.pathname !== '/'
    || url.search
    || url.hash
    || url.username
    || url.password
  ) {
    throw new Error(
      'PUBLIC_URL must be an HTTP(S) origin without a path or credentials.',
    )
  }

  settings.publicUrl = url.origin
  return settings
}
