import { execFile } from 'node:child_process'
import { constants } from 'node:fs'
import {
  access,
  copyFile,
  mkdir,
  readdir,
  readlink,
  rm,
  symlink,
} from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'
import { promisify } from 'node:util'

export async function toolkitEnvironment(home: string, base: NodeJS.ProcessEnv = process.env): Promise<NodeJS.ProcessEnv> {
  if (!base.LEO_TOOLKIT_DIR)
    return { ...base }
  const directory = base.LEO_TOOLKIT_DIR
  const rustup = path.join(home, '.rustup')
  const cargo = path.join(home, '.cargo')
  await mkdir(path.join(rustup, 'toolchains'), { recursive: true })
  await mkdir(path.join(cargo, 'bin'), { recursive: true })
  for (const folder of ['update-hashes', 'downloads', 'tmp'])
    await mkdir(path.join(rustup, folder), { recursive: true })
  for (const hash of await readdir(path.join(directory, 'rustup/update-hashes'))) {
    await copyFile(path.join(directory, 'rustup/update-hashes', hash), path.join(rustup, 'update-hashes', hash), constants.COPYFILE_EXCL).catch((error: NodeJS.ErrnoException) => {
      if (error.code !== 'EEXIST')
        throw error
    })
  }

  const link = async (source: string, target: string) => {
    await symlink(source, target).catch((error: NodeJS.ErrnoException) => {
      if (error.code !== 'EEXIST')
        throw error
    })
  }

  // A replacement image may no longer contain an intermediate tool version.
  // Remove only dangling links to our image, so that version can be installed locally.
  const prune = async (folder: string) => {
    for (const name of await readdir(folder)) {
      const file = path.join(folder, name)
      const target = await readlink(file).catch(() => '')
      if ((target.startsWith('/usr/local/share/mise/installs/') || target.startsWith(path.join(directory, 'rustup/toolchains/')))
        && !await access(file).then(() => true).catch(() => false)) {
        await rm(file)
      }
    }
  }

  await prune(path.join(rustup, 'toolchains'))
  const installs = path.join(home, '.local/share/mise/installs')
  // These backends discover idiomatic version files from the install registry.
  // Register their shared versions locally so new project pins install in the home.
  for (const tool of ['node', 'pnpm', 'python', 'go', 'rust']) {
    const source = path.join('/usr/local/share/mise/installs', tool)
    const target = path.join(installs, tool)
    await mkdir(target, { recursive: true })
    await prune(target)
    await copyFile(path.join(source, '.mise.backend.toml'), path.join(target, '.mise.backend.toml'), constants.COPYFILE_EXCL).catch((error: NodeJS.ErrnoException) => {
      if (error.code !== 'EEXIST' && error.code !== 'ENOENT')
        throw error
    })
    for (const version of await readdir(source)) {
      if (!/^\d+\.\d+\.\d+$/.test(version))
        continue
      await link(path.join(source, version), path.join(target, version))
    }
  }

  const shims = path.join(home, '.local/share/mise/shims')
  await mkdir(shims, { recursive: true })
  for (const name of await readdir('/usr/local/share/mise/shims'))
    await link('/usr/local/bin/mise', path.join(shims, name))
  for (const name of await readdir(path.join(directory, 'rustup/toolchains')))
    await link(path.join(directory, 'rustup/toolchains', name), path.join(rustup, 'toolchains', name))
  // rustup updates its own proxies when installing a project toolchain. Keep one
  // private binary rather than a proxy that resolves into the read-only image.
  for (const name of await readdir(path.join(directory, 'cargo/bin'))) {
    const target = path.join(cargo, 'bin', name)
    const existing = await readlink(target).catch(() => '')
    if (existing.startsWith(path.join(directory, 'cargo/bin/')))
      await rm(target)
    if (name === 'rustup') {
      await copyFile(path.join(directory, 'cargo/bin/rustup'), target, constants.COPYFILE_EXCL).catch((error: NodeJS.ErrnoException) => {
        if (error.code !== 'EEXIST')
          throw error
      })
    }
    else {
      await link(path.join(cargo, 'bin/rustup'), target)
    }
  }

  const settings = path.join(rustup, 'settings.toml')
  if (!await access(settings).then(() => true).catch(() => false)) {
    await copyFile(path.join(directory, 'rustup/settings.toml'), settings, constants.COPYFILE_EXCL).catch((error: NodeJS.ErrnoException) => {
      if (error.code !== 'EEXIST')
        throw error
    })
  }

  const env = {
    ...base,
    HOME: home,
    RUSTUP_HOME: rustup,
    CARGO_HOME: cargo,
    PATH: [path.join(home, '.local/share/mise/shims'), '/usr/local/share/mise/shims', path.join(cargo, 'bin'), base.PATH].filter(Boolean).join(path.delimiter),
  }
  await promisify(execFile)('/usr/local/bin/mise', ['reshim'], { cwd: '/tmp', env, timeout: 30000 })
  // Resolve the baked tools while the private home is writable. Inside a
  // read-only Codex sandbox, cache writes would otherwise flood every command.
  await promisify(execFile)('/usr/local/bin/mise', ['env', '--json'], { cwd: '/tmp', env, timeout: 30000 })
  return env
}
