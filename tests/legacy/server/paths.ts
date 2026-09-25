import { realpath, stat } from 'node:fs/promises'
import path from 'node:path'
import { AppError } from './errors.ts'

export async function workspaceDirectory(
  candidate: string,
  allowedRoots: string[],
) {
  let actual: string
  try {
    actual = await realpath(candidate)
    if (!(await stat(actual)).isDirectory())
      throw new Error('Not a directory')
  }
  catch {
    throw new AppError(
      400,
      'Project directory does not exist or is not accessible.',
    )
  }

  const roots = await Promise.all(
    allowedRoots.map(root => realpath(root).catch(() => null)),
  )
  if (
    !roots.some(
      root => root && (actual === root || actual.startsWith(root + path.sep)),
    )
  ) {
    throw new AppError(
      400,
      'Project must be inside a configured workspace root.',
    )
  }

  return actual
}
