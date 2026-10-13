import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from 'vitest'

const request = vi.hoisted(() => vi.fn())
vi.mock('../src/api', () => ({ api: request, state: { installationId: 'fixture-installation' } }))

beforeEach(() => {
  vi.resetModules()
  request.mockReset()
  const values = new Map<string, string>()
  vi.stubGlobal('localStorage', {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
  })
})

afterEach(() => vi.unstubAllGlobals())

describe('coding-agent model catalogs', () => {
  it('deduplicates requests per engine while refreshing different engines independently', async () => {
    let resolveCodex!: (value: unknown) => void
    const codex = new Promise(resolve => resolveCodex = resolve)
    request.mockImplementation((path: string) => path === '/codex/models'
      ? codex
      : Promise.resolve({
          models: [{ model: 'fixture-claude' }],
          checkedAt: 2,
          stale: false,
          error: '',
        }))
    const { modelCatalogs } = await import('../src/models')
    const first = modelCatalogs.codex.load()
    expect(modelCatalogs.codex.load()).toBe(first)
    await modelCatalogs.claude.load()
    expect(modelCatalogs.codex.catalog.loading).toBe(true)
    expect(modelCatalogs.claude.catalog.models[0].model).toBe('fixture-claude')
    resolveCodex({
      models: [{ model: 'fixture-codex' }],
      checkedAt: 1,
      stale: false,
      error: '',
    })
    await first
    expect(request.mock.calls.map(([path]) => path)).toEqual(['/codex/models', '/claude/models'])
    expect(modelCatalogs.codex.catalog.models[0].model).toBe('fixture-codex')
    expect(modelCatalogs.codex.catalog.loading).toBe(false)
    expect(localStorage.getItem('fixture-installation:cairn-models:codex')).toContain('fixture-codex')
    expect(localStorage.getItem('fixture-installation:cairn-models:claude')).toContain('fixture-claude')
  })

  it('preserves saved model labels after a failed refresh and allows retry', async () => {
    localStorage.setItem('fixture-installation:cairn-models:claude', JSON.stringify({ models: [{ model: 'saved-claude' }] }))
    request.mockRejectedValueOnce(new Error('Fixture unavailable'))
    const { modelCatalogs } = await import('../src/models')
    await modelCatalogs.claude.load()
    expect(modelCatalogs.claude.catalog.models[0].model).toBe('saved-claude')
    expect(modelCatalogs.claude.catalog.error).toBe('Fixture unavailable')
    expect(modelCatalogs.codex.catalog.models).toEqual([])
    request.mockResolvedValueOnce({
      models: [{ model: 'new-claude' }],
      checkedAt: 3,
      stale: false,
      error: '',
    })
    await modelCatalogs.claude.load()
    expect(modelCatalogs.claude.catalog.models[0].model).toBe('new-claude')
    expect(modelCatalogs.claude.catalog.error).toBe('')
  })
})
