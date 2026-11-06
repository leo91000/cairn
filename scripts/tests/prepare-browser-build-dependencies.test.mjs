import { describe, expect, it } from 'vitest'
import { canonicalBrowserUbuntuSources, prepareBrowserBuildDependencies } from '../prepare-browser-build-dependencies.mjs'

const sources = `# Preserve the Ubuntu signature configuration.
Types: deb
URIs: http://azure.archive.ubuntu.com/ubuntu/
Suites: noble noble-updates noble-backports
Components: main universe restricted multiverse
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg

Types: deb
URIs: http://security.ubuntu.com/ubuntu/
Suites: noble-security
Components: main universe restricted multiverse
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg
`

function fixture({ status = 0, os = 'ID=ubuntu\nVERSION_ID="24.04"\n', input = sources } = {}) {
  const writes = []
  const calls = []
  return {
    writes,
    calls,
    options: {
      read: name => name === '/etc/os-release' ? os : input,
      write: (...args) => writes.push(args),
      run: (...args) => {
        calls.push(args)
        return { status }
      },
    },
  }
}

describe('browser Ubuntu dependency bootstrap', () => {
  it('changes only the known Ubuntu transport endpoints and keeps authenticated pockets intact', () => {
    const normalized = canonicalBrowserUbuntuSources(sources)
    expect(normalized).toBe(sources
      .replace('http://azure.archive.ubuntu.com', 'https://archive.ubuntu.com')
      .replace('http://security.ubuntu.com', 'https://security.ubuntu.com'))
    expect(canonicalBrowserUbuntuSources(normalized)).toBe(normalized)
    expect(canonicalBrowserUbuntuSources(sources.replace('azure.archive', 'archive'))).toBe(normalized)
  })

  it.each([
    ['unknown host', sources.replace('azure.archive.ubuntu.com', 'mirror.example.test')],
    ['HTTP credentials', sources.replace('azure.archive.ubuntu.com', 'user:pass@azure.archive.ubuntu.com')],
    ['extra URL', sources.replace('http://azure.archive.ubuntu.com/ubuntu/', 'http://azure.archive.ubuntu.com/ubuntu/ https://mirror.example.test/ubuntu/')],
    ['continued URL', sources.replace('http://azure.archive.ubuntu.com/ubuntu/', 'http://azure.archive.ubuntu.com/ubuntu/\n https://mirror.example.test/ubuntu/')],
    ['continued URL after comment', sources.replace('http://azure.archive.ubuntu.com/ubuntu/', 'http://azure.archive.ubuntu.com/ubuntu/\n# mirror comment\n https://mirror.example.test/ubuntu/')],
    ['duplicate pocket', `${sources}\nURIs: https://archive.ubuntu.com/ubuntu/\n`],
    ['unknown lowercase field', `${sources}\nuris: https://mirror.example.test/ubuntu/\n`],
    ['missing security source', sources.slice(0, sources.indexOf('\n\n'))],
    ['different path', sources.replace('/ubuntu/', '/other/')],
  ])('refuses %s before any source write or package execution', (_name, input) => {
    const value = fixture({ input })
    expect(() => prepareBrowserBuildDependencies(value.options)).toThrow('CI_BROWSER_PACKAGES_UNSUPPORTED_SOURCES')
    expect(value.writes).toEqual([])
    expect(value.calls).toEqual([])
  })

  it('retains the dependency set and puts update plus installation in one owned deadline', () => {
    const value = fixture()
    prepareBrowserBuildDependencies(value.options)
    expect(value.writes).toEqual([['/etc/apt/sources.list.d/ubuntu.sources', canonicalBrowserUbuntuSources(sources)]])
    expect(value.calls).toHaveLength(1)
    const [command, args, options] = value.calls[0]
    expect(command).toBe('timeout')
    expect(args.slice(0, 6)).toEqual(['--signal=TERM', '--kill-after=5s', '180s', 'bash', '-euc', expect.any(String)])
    expect(args[5]).toContain('APT::Update::Error-Mode=any update -qq')
    expect(args[5]).toContain('install -y --no-install-recommends build-essential curl libclang-dev libssl-dev pkg-config')
    expect(args[5]).toContain('Acquire::Retries=0')
    expect(args[5]).not.toContain('allow-unauthenticated')
    expect(options).toEqual({ stdio: 'inherit' })
  })

  it.each([1, 100, 124, 137, null])('does not turn apt or supervisor status %s into success', (status) => {
    const value = fixture({ status })
    expect(() => prepareBrowserBuildDependencies(value.options)).toThrow(status === 124 || status === 137 ? 'CI_BROWSER_PACKAGES_TIMEOUT' : 'CI_BROWSER_PACKAGES_FAILED')
  })

  it.each([0, -1, Number.NaN, 181])('refuses invalid or extended diagnostic budget %s', (timeoutSeconds) => {
    const value = fixture()
    expect(() => prepareBrowserBuildDependencies({ ...value.options, timeoutSeconds })).toThrow('CI_BROWSER_PACKAGES_INVALID_BUDGET')
    expect(value.writes).toEqual([])
    expect(value.calls).toEqual([])
  })

  it('refuses another operating system before changing its package sources', () => {
    const value = fixture({ os: 'ID=arch\n' })
    expect(() => prepareBrowserBuildDependencies(value.options)).toThrow('CI_BROWSER_PACKAGES_UNSUPPORTED_HOST')
    expect(value.writes).toEqual([])
    expect(value.calls).toEqual([])
  })
})
