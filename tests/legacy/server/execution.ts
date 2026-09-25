import type { Config } from './config.ts'
import type { Run } from './contracts.ts'
import { execFile } from 'node:child_process'
import { randomBytes } from 'node:crypto'
import {
  access,
  copyFile,
  cp,
  lstat,
  mkdir,
  readdir,
  readFile,
  realpath,
  symlink,
  writeFile,
} from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'
import { promisify } from 'node:util'
import YAML from 'yaml'
import { AppError } from './errors.ts'
import { workspaceDirectory } from './paths.ts'
import { isolated, policy, runProjects } from './policy.ts'

const exec = promisify(execFile)
export interface RunnerPlan {
  chat?: import('./chat-process.ts').ChatPlan
  id: string
  args: string[]
  cwd: string
  prompt: string
  mounts: { source: string, target: string, readOnly: boolean }[]
  expires: number | null
  sandbox: string
  mcpEnv?: { LEO_MCP_RUN_TOKEN?: string }
}
export async function runnerSecret(dataDir: string) {
  const filename = path.join(dataDir, 'runner-secret')
  try {
    await writeFile(filename, randomBytes(32).toString('hex'), { flag: 'wx', mode: 0o600 })
  }
  catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'EEXIST')
      throw error
  }

  return (await readFile(filename, 'utf8')).trim()
}

async function copyTree(source: string, target: string) {
  // Skill resources must stay inside their selected skill; do not copy escaping links.
  for (const name of await readdir(source, { recursive: true })) {
    if ((await lstat(path.join(source, name))).isSymbolicLink())
      throw new AppError(400, 'Selected skill resources must not contain symbolic links.')
  }

  await cp(source, target, { recursive: true, dereference: false })
}

export async function prepareExecution(run: Run, config: Config, githubToken?: string, codexHome?: string, generation?: string) {
  const directory = path.join(config.dataDir, 'runs', run.id)
  const isIsolated = isolated(run.snapshot.agent)
  const accessPolicy = policy(run.snapshot.agent)
  if (isIsolated && !config.runnerUrl)
    throw new AppError(503, 'Isolated runner is not configured. This agent will not fall back to shared execution.')
  const root = path.join(directory, generation ? `workspace-${generation}` : 'workspace')
  await mkdir(root, { recursive: true, mode: 0o700 })
  const projects = runProjects(run)
  const workspaces: NonNullable<Run['workspaces']> = []
  const mounts: RunnerPlan['mounts'] = []
  for (const project of projects) {
    const source = await workspaceDirectory(project.path, config.workspaceRoots)
    if (source !== project.path)
      throw new AppError(400, 'Project directory changed location after this run was queued.')
    let target = source
    let kind: 'clone' | 'worktree' | 'direct' = 'direct'
    if (run.snapshot.task.worktree) {
      // Retain the historical single-worktree layout for unrestricted runs.
      target = !isIsolated && projects.length === 1 ? root : path.join(root, project.id)
      if (isIsolated) {
        kind = 'clone'
        // Registered source checkouts are trusted: let upload-pack hydrate a partial
        // clone from its promisor remote. Keep the run's clone fully independent.
        await exec('git', ['clone', '--no-hardlinks', '--no-local', '--branch', project.baseBranch, source, target], {
          timeout: 120000,
          maxBuffer: 100000,
          env: { ...process.env, GIT_NO_LAZY_FETCH: '0' },
        })
        const remote = await exec('git', ['-C', source, 'config', '--get', 'remote.origin.url'], { timeout: 10000 }).catch(() => undefined)
        if (remote?.stdout.trim()) {
          let url = remote.stdout.trim()
          if (/^https?:/.test(url)) {
            const parsed = new URL(url)
            parsed.username = ''
            parsed.password = ''
            url = parsed.toString()
          }

          await exec('git', ['-C', target, 'remote', 'set-url', 'origin', url], { timeout: 10000 })
        }
      }
      else {
        kind = 'worktree'
        if (target === root)
          await import('node:fs/promises').then(fs => fs.rmdir(root))
        await exec('git', ['-C', source, 'worktree', 'add', '-b', `feat/run-${run.id.slice(0, 8)}-${project.id.slice(0, 8)}${generation ? `-${generation}` : ''}`, target, project.baseBranch], { timeout: 30000, maxBuffer: 100000 })
      }
    }

    workspaces.push({ projectId: project.id, path: target, kind })
    if (isIsolated)
      mounts.push({ source: target, target, readOnly: accessPolicy.sandbox === 'read-only' })
  }

  const cwd = workspaces.length === 1 ? workspaces[0].path : root
  const outputDirectory = path.join(directory, 'output')
  await mkdir(outputDirectory, { recursive: true, mode: 0o700 })
  const output = path.join(outputDirectory, 'result.md')
  if (!isIsolated) {
    return {
      cwd,
      output,
      workspaces,
      isolated: false,
      mounts,
      skills: run.snapshot.skills,
    }
  }

  const home = path.join(directory, 'home')
  await mkdir(path.join(home, '.codex'), { recursive: true, mode: 0o700 })
  const auth = path.join(codexHome ?? path.join(config.home, '.codex'), 'auth.json')
  await access(auth).catch(() => {
    throw new AppError(400, 'Connect Codex before starting an isolated agent.')
  })
  await copyFile(await realpath(auth), path.join(home, '.codex', 'auth.json'))
  await writeFile(path.join(home, '.codex', 'config.toml'), 'cli_auth_credentials_store = "file"\n', { mode: 0o600 })
  await prepareGithubHome(home, config, accessPolicy.github, githubToken)
  const gitConfig = path.join(home, '.gitconfig')
  await exec('git', ['config', '--file', gitConfig, 'user.name', run.snapshot.agent.name])
  await exec('git', ['config', '--file', gitConfig, 'user.email', 'agent@localhost'])
  if (githubToken || accessPolicy.github)
    await exec('git', ['config', '--file', gitConfig, 'credential.https://github.com.helper', '!gh auth git-credential'])
  const skills: Run['snapshot']['skills'] = []
  for (const [index, skill] of run.snapshot.skills.entries()) {
    const target = path.join(home, '.agents', 'skills', `${index}/${skill.name}`)
    await mkdir(path.dirname(target), { recursive: true })
    await copyTree(path.dirname(skill.path), target)
    // The run's queued instructions are immutable; supporting files come from the selected directory.
    await writeFile(path.join(target, 'SKILL.md'), skill.content)
    skills.push({ ...skill, path: path.join('/home/node/.agents/skills', `${index}/${skill.name}`, 'SKILL.md') })
  }

  mounts.unshift({ source: root, target: root, readOnly: false })
  mounts.push({ source: home, target: '/home/node', readOnly: false }, { source: outputDirectory, target: outputDirectory, readOnly: false })
  const empty = path.join(directory, 'empty')
  await mkdir(empty, { recursive: true })
  for (const workspace of workspaces) {
    // Project config must not re-enable shared MCP connections or unselected skills.
    for (const relative of ['.codex', '.agents/skills']) {
      if (await access(path.join(workspace.path, relative)).then(() => true).catch(() => false))
        mounts.push({ source: empty, target: path.join(workspace.path, relative), readOnly: true })
    }
  }

  return {
    cwd,
    output,
    workspaces,
    isolated: true,
    mounts,
    skills,
  }
}

// Unrestricted runs retain user configuration while auth and session state stay per run.
export async function prepareCodexHome(config: Config, home: string) {
  const source = path.join(config.home, '.codex')
  for (const file of ['config.toml', 'AGENTS.md']) {
    await copyFile(path.join(source, file), path.join(home, file)).catch((error) => {
      if (error.code !== 'ENOENT')
        throw error
    })
  }

  for (const directory of ['rules', 'skills', 'plugins']) {
    if (!await access(path.join(source, directory)).then(() => true).catch(() => false))
      continue
    await symlink(path.join(source, directory), path.join(home, directory), 'dir').catch((error) => {
      if (error.code !== 'EEXIST')
        throw error
    })
  }
}

export async function restoreExecution(run: Run, prepared: Awaited<ReturnType<typeof prepareExecution>>, config: Config, githubToken?: string) {
  if (prepared.isolated !== isolated(run.snapshot.agent))
    throw new AppError(409, 'Execution isolation changed; this run cannot be resumed.')
  if (prepared.isolated && !config.runnerUrl)
    throw new AppError(503, 'The isolated runner is not configured.')
  for (const project of runProjects(run)) {
    if (await workspaceDirectory(project.path, config.workspaceRoots) !== project.path)
      throw new AppError(409, 'A project moved outside its permitted location.')
  }

  const root = path.join(config.dataDir, 'runs', run.id)
  const allowed = [root, ...prepared.workspaces.filter(workspace => workspace.kind === 'direct').map(workspace => requireProject(run, workspace.projectId))]
  for (const source of new Set([prepared.cwd, path.dirname(prepared.output), ...prepared.workspaces.map(workspace => workspace.path), ...prepared.mounts.map(mount => mount.source)])) {
    if (await workspaceDirectory(source, allowed) !== source)
      throw new AppError(409, 'A saved workspace changed location. Working files were preserved.')
  }

  if (prepared.isolated)
    await prepareGithubHome(path.join(root, 'home'), config, policy(run.snapshot.agent).github, githubToken)
  return prepared
}

function requireProject(run: Run, id: string) {
  const project = runProjects(run).find(project => project.id === id)
  if (!project)
    throw new AppError(409, 'A saved workspace is no longer in the agent’s project scope.')
  return project.path
}

async function prepareGithubHome(home: string, config: Config, shared: boolean, githubToken?: string) {
  // Share only the explicitly enabled GitHub connection, never the entire user home.
  if (shared) {
    const github = path.join(config.home, '.config', 'gh')
    if (await access(github).then(() => true).catch(() => false))
      await cp(github, path.join(home, '.config', 'gh'), { recursive: true })
  }

  if (githubToken && !shared) {
    await mkdir(path.join(home, '.config', 'gh'), { recursive: true })
    await writeFile(path.join(home, '.config', 'gh', 'hosts.yml'), YAML.stringify({ 'github.com': { oauth_token: githubToken, git_protocol: 'https' } }), { mode: 0o600 })
  }
}
