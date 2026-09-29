import type { Skill } from '../../../shared/contracts.ts'
import { randomUUID } from 'node:crypto'
import {
  lstat,
  mkdir,
  readdir,
  readFile,
  realpath,
  rename,
  rm,
  writeFile,
} from 'node:fs/promises'
import path from 'node:path'
import YAML from 'yaml'
import { AppError } from './errors.ts'
import { workspaceDirectory } from './paths.ts'

export function skillName(name: string) {
  if (!/^[a-z0-9][a-z0-9-]{0,63}$/.test(name)) {
    throw new AppError(
      400,
      'Use a skill name with lowercase letters, numbers, and hyphens.',
    )
  }

  return name
}

export function parseSkill(content: string) {
  if (content.length > 100000)
    throw new AppError(400, 'Skill is too large (maximum 100 KB).')
  const match = /^---\r?\n([\s\S]*?)\r?\n---(?:\r?\n|$)/.exec(content)
  if (!match) {
    throw new AppError(
      400,
      'SKILL.md needs YAML frontmatter with name and description.',
    )
  }

  let data
  try {
    data = YAML.parse(match[1])
  }
  catch {
    throw new AppError(400, 'Invalid YAML frontmatter.')
  }

  if (
    !data
    || typeof data.name !== 'string'
    || typeof data.description !== 'string'
    || !data.description.trim()
  ) {
    throw new AppError(
      400,
      'Add a name and description to the skill frontmatter.',
    )
  }

  skillName(data.name)
  return { name: data.name, description: data.description }
}

export async function boundedPath(
  root: string,
  relative: string,
  { missing = false } = {},
) {
  if (
    typeof relative !== 'string'
    || path.isAbsolute(relative)
    || relative.split(/[\\/]/).some(p => p === '..' || p === '' || p === '.')
    || relative.includes('\0')
  ) {
    throw new AppError(400, 'Invalid file path.')
  }

  const base = await realpath(root)
  const target = path.resolve(base, relative)
  let resolved: string
  try {
    resolved = await realpath(target)
  }
  catch (e) {
    if (!missing || (e as NodeJS.ErrnoException).code !== 'ENOENT')
      throw e
    resolved = path.join(
      await realpath(path.dirname(target)),
      path.basename(target),
    )
  }

  if (!resolved.startsWith(base + path.sep))
    throw new AppError(400, 'File path escapes its skill directory.')
  return resolved
}

export class Skills {
  constructor(
    public home: string,
    public workspaceRoots: string[],
  ) {}

  async root(projectPath?: string) {
    let current = projectPath
      ? await workspaceDirectory(projectPath, this.workspaceRoots)
      : await realpath(this.home)
    for (const segment of ['.agents', 'skills']) {
      current = path.join(current, segment)
      await mkdir(current, { mode: 0o700 }).catch(
        (error: NodeJS.ErrnoException) => {
          if (error.code !== 'EEXIST')
            throw error
        },
      )
      const info = await lstat(current)
      if (!info.isDirectory() || info.isSymbolicLink()) {
        throw new AppError(
          400,
          'The .agents/skills path must use real directories, not symbolic links.',
        )
      }
    }

    return current
  }

  async list(scope = 'global', projectPath?: string): Promise<Skill[]> {
    const root = await this.root(projectPath)
    await mkdir(root, { recursive: true, mode: 0o700 })
    const entries = await readdir(root, { withFileTypes: true })
    const result: Skill[] = []
    for (const entry of entries) {
      if (!entry.isDirectory())
        continue
      try {
        skillName(entry.name)
        const file = await boundedPath(root, `${entry.name}/SKILL.md`)
        if ((await lstat(file)).size > 100000)
          throw new AppError(400, 'Skill is too large (maximum 100 KB).')
        const content = await readFile(file, 'utf8')
        try {
          const parsed = parseSkill(content)
          result.push({
            ...parsed,
            scope,
            path: file,
            content,
            valid: parsed.name === entry.name,
            error:
              parsed.name === entry.name
                ? undefined
                : 'Name differs from directory',
          })
        }
        catch (e) {
          result.push({
            name: entry.name,
            description: 'Invalid skill',
            scope,
            path: file,
            content,
            valid: false,
            error: (e as Error).message,
          })
        }
      }
      catch (error) {
        result.push({
          name: entry.name,
          description: 'Invalid skill',
          scope,
          path: path.join(root, entry.name, 'SKILL.md'),
          content: '',
          valid: false,
          error: (error as Error).message,
        })
      }
    }

    return result.sort((a, b) => a.name.localeCompare(b.name))
  }

  async save(name: string, content: string, projectPath?: string) {
    skillName(name)
    const parsed = parseSkill(content)
    if (parsed.name !== name) {
      throw new AppError(
        400,
        'Frontmatter name must match the skill directory.',
      )
    }

    const root = await this.root(projectPath)
    await mkdir(root, { recursive: true, mode: 0o700 })
    const directory = await boundedPath(root, name, { missing: true })
    await mkdir(directory, { recursive: true })
    const file = await boundedPath(root, `${name}/SKILL.md`, { missing: true })
    const tmp = path.join(directory, `.${randomUUID()}.tmp`)
    await writeFile(tmp, content, { mode: 0o600 })
    await rename(tmp, file)
    return parsed
  }

  async remove(name: string, projectPath?: string) {
    skillName(name)
    const directory = await boundedPath(await this.root(projectPath), name)
    await rm(directory, { recursive: true })
  }

  async files(name: string, projectPath?: string) {
    const root = await boundedPath(
      await this.root(projectPath),
      skillName(name),
    )
    const result: string[] = []
    const visit = async (relative: string) => {
      for (const entry of await readdir(path.join(root, relative), {
        withFileTypes: true,
      })) {
        if (result.length >= 200)
          return
        if (entry.name.startsWith('.') || entry.isSymbolicLink())
          continue
        const file = path.join(relative, entry.name)
        if (entry.isDirectory()) {
          if (file.split(path.sep).length < 5)
            await visit(file)
        }
        else if (result.length < 200) {
          result.push(file)
        }
      }
    }

    await visit('')
    return result
  }

  async file(
    name: string,
    file: string,
    content: string | undefined,
    projectPath?: string,
  ) {
    const root = await boundedPath(
      await this.root(projectPath),
      skillName(name),
    )
    if (content !== undefined) {
      // Validate before creating any supporting directories.
      if (
        typeof file !== 'string'
        || path.isAbsolute(file)
        || file
          .split(/[\\/]/)
          .some(part => !part || part === '.' || part === '..')
          || file.includes('\0')
      ) {
        throw new AppError(400, 'Invalid file path.')
      }

      let directory = root
      for (const part of file.split('/').slice(0, -1)) {
        directory = await boundedPath(directory, part, { missing: true })
        await mkdir(directory, { recursive: true })
        // Check against the original skill root after each filesystem operation.
        await boundedPath(root, path.relative(root, directory))
      }
    }

    const target = await boundedPath(root, file, {
      missing: content !== undefined,
    })
    if (content === undefined) {
      if ((await lstat(target)).size > 100000)
        throw new AppError(400, 'File is too large.')
      return readFile(target, 'utf8')
    }

    if (content.length > 100000)
      throw new AppError(400, 'File is too large.')
    if (file === 'SKILL.md')
      return this.save(name, content, projectPath)
    await writeFile(target, content, { mode: 0o600 })
    return { saved: true }
  }
}
