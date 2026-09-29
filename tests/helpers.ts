import type { Config } from './legacy/server/config.ts'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { buildApp } from './legacy/server/app.ts'

export async function fixture(overrides: Partial<Config> = {}) {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'leo-manager-test-'))
  const home = path.join(directory, 'home')
  const projectPath = path.join(directory, 'project')
  await Promise.all([mkdir(home), mkdir(projectPath)])
  const context = await buildApp({
    dataDir: path.join(directory, 'data'),
    home,
    workspaceRoots: [directory],
    logger: false,
    workerEnabled: false,
    setupToken: 'test-setup-token',
    ...overrides,
  })
  const { service, app } = context
  const agent = service.agent({ name: 'Test agent' })
  const project = await service.project({
    name: 'Test project',
    path: projectPath,
  })
  const task = service.task({
    name: 'Test task',
    prompt: 'Complete the fixture task.',
    agentId: agent.id,
    projectId: project.id,
    worktree: false,
  })
  const login = async () => {
    const response = await app.inject({
      method: 'POST',
      url: '/api/setup',
      payload: {
        setupToken: 'test-setup-token',
        password: 'test-password-long-enough',
      },
    })
    if (response.statusCode !== 200)
      throw new Error(response.body)
    return {
      'cookie': response.headers['set-cookie']!.toString().split(';')[0],
      'x-csrf-token': response.json().csrf as string,
    }
  }

  const dispose = async () => {
    await app.close()
    await rm(directory, { recursive: true, force: true })
  }

  return {
    ...context,
    directory,
    home,
    projectPath,
    agent,
    project,
    task,
    login,
    dispose,
  }
}
