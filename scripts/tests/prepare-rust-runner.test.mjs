import { spawnSync } from 'node:child_process'
import { readFileSync } from 'node:fs'
import process from 'node:process'
import { fileURLToPath } from 'node:url'
import {
  describe,
  expect,
  it,
  vi,
} from 'vitest'
import { parse } from 'yaml'
import { prepareRustRunner, REQUIRED_FREE_BYTES, UNUSED_SDK_DIRECTORIES } from '../prepare-rust-runner.mjs'

const environment = {
  GITHUB_ACTIONS: 'true',
  RUNNER_ENVIRONMENT: 'github-hosted',
  RUNNER_OS: 'Linux',
  ImageOS: 'ubuntu24',
  ImageVersion: '20261101.114.1',
  GITHUB_JOB: 'backend',
  GITHUB_WORKSPACE: '/home/runner/work/cairn/cairn',
  RUNNER_TEMP: '/home/runner/work/_temp',
  HOME: '/home/runner',
}

function fixture(initial = 15913492480n) {
  let free = initial
  const reclaimed = [11838935040n, 6167736320n, 3710808064n]
  const stat = {
    dev: 1,
    uid: 0,
    isDirectory: () => true,
    isSymbolicLink: () => false,
  }
  const filesystem = {
    realpathSync: vi.fn(location => location),
    statSync: vi.fn(() => stat),
    lstatSync: vi.fn(() => stat),
    statfsSync: vi.fn(() => ({ bavail: free, bsize: 1n })),
  }
  const execute = vi.fn((command, args) => {
    if (command === 'which')
      return args.map(tool => `/usr/bin/${tool}`).join('\n')
    const index = UNUSED_SDK_DIRECTORIES.indexOf(args.at(-1))
    if (index < 0)
      throw new Error('Cleanup escaped its fixed SDK allowlist')
    free += reclaimed[index]
    return ''
  })
  return {
    filesystem,
    execute,
    platform: 'linux',
    userInfo: () => ({ username: 'runner', homedir: '/home/runner' }),
  }
}

describe('disposable Rust test runner disk preparation', () => {
  it('prepares only the two disposable Ubuntu Rust test jobs before their build observations', () => {
    const workflow = parse(readFileSync(new URL('../../.github/workflows/ci.yaml', import.meta.url), 'utf8'))
    const callers = Object.entries(workflow.jobs).filter(([, job]) => job.steps?.some(step => step.uses === './.github/actions/prepare-rust-runner'))
    expect(callers.map(([name]) => name).sort()).toEqual(['backend', 'cli-qualification'])
    for (const [, job] of callers) {
      expect(job['runs-on']).toBe('ubuntu-latest')
      expect(job.container).toBeUndefined()
      const preparation = job.steps.findIndex(step => step.uses === './.github/actions/prepare-rust-runner')
      const dependencies = job.steps.findIndex(step => step.name === 'Install native test dependencies')
      const observation = job.steps.findIndex(step => step.uses === './.github/actions/observe-rust-build' && step.with?.phase === 'before-build')
      expect(job.steps[preparation]['timeout-minutes']).toBe(2)
      expect(preparation).toBeGreaterThan(dependencies)
      expect(preparation).toBeLessThan(observation)
    }
  })

  it.each(['backend', 'cli-qualification'])('reclaims measured unused SDKs for %s before admitting a standard-sized runner', (job) => {
    const dependencies = fixture()
    const result = prepareRustRunner({ ...environment, GITHUB_JOB: job }, dependencies)
    expect(result.beforeBytes).toBe('15913492480')
    expect(BigInt(result.afterBytes)).toBeGreaterThanOrEqual(REQUIRED_FREE_BYTES)
    expect(result.directories).toEqual(UNUSED_SDK_DIRECTORIES.map(path => ({ path, status: 'removed' })))
    expect(dependencies.execute.mock.calls.filter(([command]) => command === 'sudo')).toEqual(
      UNUSED_SDK_DIRECTORIES.map(directory => [
        'sudo',
        ['-n', 'rm', '--recursive', '--force', '--one-file-system', '--', directory],
        { stdio: 'inherit', timeout: expect.any(Number) },
      ]),
    )
  })

  it.each([
    { GITHUB_ACTIONS: undefined },
    { RUNNER_ENVIRONMENT: 'self-hosted' },
    { RUNNER_OS: 'Windows' },
    { GITHUB_JOB: 'android-direct' },
    { GITHUB_JOB: 'quality' },
    { ImageOS: 'windows2025' },
    { ImageVersion: 'unknown' },
  ])('rejects unsupported contexts before any filesystem access or command: %j', (change) => {
    const dependencies = fixture()
    expect(() => prepareRustRunner({ ...environment, ...change }, dependencies)).toThrow('GitHub-hosted Ubuntu test job')
    expect(dependencies.filesystem.realpathSync).not.toHaveBeenCalled()
    expect(dependencies.execute).not.toHaveBeenCalled()
  })

  it('refuses a local account even when a caller copied the CI environment', () => {
    const dependencies = fixture()
    dependencies.userInfo = () => ({ username: 'desktop-user', homedir: '/home/desktop-user' })
    expect(() => prepareRustRunner(environment, dependencies)).toThrow('runner account')
    expect(dependencies.execute).not.toHaveBeenCalled()
  })

  it.each([
    { HOME: '/home/another-account' },
    { RUNNER_TEMP: 'relative-temp' },
    { CARGO_TARGET_DIR: '/mnt/shared-target' },
    { CARGO_HOME: '/usr/share/dotnet/cargo' },
    { GITHUB_WORKSPACE: '/usr/local/lib/android/repository' },
  ])('protects user and build paths from reclamation: %j', (change) => {
    const dependencies = fixture()
    expect(() => prepareRustRunner({ ...environment, ...change }, dependencies)).toThrow()
    expect(dependencies.execute).not.toHaveBeenCalled()
  })

  it('does not treat space on another build filesystem as workspace capacity', () => {
    const dependencies = fixture()
    dependencies.filesystem.statSync.mockImplementation(location => ({ dev: location === environment.RUNNER_TEMP ? 2 : 1 }))
    expect(() => prepareRustRunner(environment, dependencies)).toThrow('share the measured filesystem')
    expect(dependencies.execute).not.toHaveBeenCalled()
  })

  it.each([
    { isSymbolicLink: () => true },
    { isDirectory: () => false },
    { uid: 1001 },
  ])('does not remove an unexpected preinstalled root: %j', (change) => {
    const dependencies = fixture()
    dependencies.filesystem.lstatSync.mockReturnValue({
      dev: 1,
      uid: 0,
      isDirectory: () => true,
      isSymbolicLink: () => false,
      ...change,
    })
    expect(() => prepareRustRunner(environment, dependencies)).toThrow('unexpected preinstalled SDK directory')
    expect(dependencies.execute.mock.calls.some(([command]) => command === 'sudo')).toBe(false)
  })

  it('refuses a parent-path symlink instead of following it into unrelated data', () => {
    const dependencies = fixture()
    dependencies.filesystem.realpathSync.mockImplementation(location => location === UNUSED_SDK_DIRECTORIES[0] ? '/home/runner/private-data' : location)
    expect(() => prepareRustRunner(environment, dependencies)).toThrow('unexpected preinstalled SDK directory')
    expect(dependencies.execute.mock.calls.some(([command]) => command === 'sudo')).toBe(false)
  })

  it('does not remove an active native compiler toolchain', () => {
    const dependencies = fixture()
    dependencies.execute.mockImplementation((command, args) => {
      if (command === 'which')
        return args.map(tool => `/usr/local/lib/android/bin/${tool}`).join('\n')
      throw new Error('Should not remove an active toolchain')
    })
    expect(() => prepareRustRunner(environment, dependencies)).toThrow('active native build tool')
    expect(dependencies.execute.mock.calls.some(([command]) => command === 'sudo')).toBe(false)
  })

  it.each([
    { CC: '/usr/share/swift/usr/bin/clang' },
    { LIBCLANG_PATH: '/usr/share/swift/usr/lib' },
    { RUSTFLAGS: '-C linker=/usr/share/swift/usr/bin/clang' },
  ])('preserves explicitly configured native toolchain inputs: %j', (change) => {
    const dependencies = fixture()
    expect(() => prepareRustRunner({ ...environment, ...change }, dependencies)).toThrow('stock native tools')
    expect(dependencies.execute).not.toHaveBeenCalled()
  })

  it('fails closed for a library alias into an otherwise unused SDK', () => {
    const dependencies = fixture()
    dependencies.filesystem.realpathSync.mockImplementation(location => location === '/opt/active-libclang' ? '/usr/share/swift/usr/lib' : location)
    expect(() => prepareRustRunner({ ...environment, LIBCLANG_PATH: '/opt/active-libclang' }, dependencies)).toThrow('explicit LIBCLANG_PATH')
    expect(dependencies.execute).not.toHaveBeenCalled()
  })

  it.each([
    { CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER: '/usr/share/swift/usr/bin/clang' },
    { CC_x86_64_unknown_linux_gnu: '/opt/active-clang' },
  ])('preserves target-specific native overrides without guessing their paths: %j', (change) => {
    const dependencies = fixture()
    expect(() => prepareRustRunner({ ...environment, ...change }, dependencies)).toThrow('stock native tools')
    expect(dependencies.execute).not.toHaveBeenCalled()
  })

  it('accepts absent SDKs without inventing reclaimed bytes when capacity is already sufficient', () => {
    const dependencies = fixture(REQUIRED_FREE_BYTES)
    dependencies.filesystem.lstatSync.mockImplementation(() => {
      throw Object.assign(new Error('absent'), { code: 'ENOENT' })
    })
    const result = prepareRustRunner(environment, dependencies)
    expect(result.beforeBytes).toBe(result.afterBytes)
    expect(result.directories.every(item => item.status === 'absent')).toBe(true)
    expect(dependencies.execute.mock.calls.some(([command]) => command === 'sudo')).toBe(false)
  })

  it('fails before compilation when no eligible SDK can provide the required workspace space', () => {
    const dependencies = fixture()
    dependencies.filesystem.lstatSync.mockReturnValue({
      dev: 2,
      uid: 0,
      isDirectory: () => true,
      isSymbolicLink: () => false,
    })
    expect(() => prepareRustRunner(environment, dependencies)).toThrow('available bytes')
    expect(dependencies.execute.mock.calls.some(([command]) => command === 'sudo')).toBe(false)
  })

  it('preserves removal errors and stops rather than claiming capacity was reclaimed', () => {
    const dependencies = fixture()
    const locate = dependencies.execute.getMockImplementation()
    dependencies.execute.mockImplementation((command, args) => {
      if (command === 'which')
        return locate(command, args)
      throw new Error('permission denied')
    })
    expect(() => prepareRustRunner(environment, dependencies)).toThrow('permission denied')
  })

  it('bounds all SDK removals with one preparation deadline', () => {
    const dependencies = fixture()
    let time = 0
    dependencies.now = () => time
    const execute = dependencies.execute.getMockImplementation()
    dependencies.execute.mockImplementation((command, args, options) => {
      const result = execute(command, args, options)
      if (command === 'sudo')
        time += 35000
      return result
    })
    expect(() => prepareRustRunner(environment, dependencies)).toThrow('60-second preparation budget')
    const removals = dependencies.execute.mock.calls.filter(([command]) => command === 'sudo')
    expect(removals).toHaveLength(2)
    expect(removals[0][2].timeout).toBe(60000)
    expect(removals[1][2].timeout).toBe(25000)
  })

  it('the real CLI refuses non-Actions execution, including inherited host metadata', () => {
    const result = spawnSync(process.execPath, [fileURLToPath(new URL('../prepare-rust-runner.mjs', import.meta.url))], {
      env: { ...process.env, GITHUB_ACTIONS: 'false' },
      encoding: 'utf8',
    })
    expect(result.status).toBe(1)
    expect(result.stderr).toContain('GitHub-hosted Ubuntu test job')
  })
})
