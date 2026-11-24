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
  it('refreshes saved preferences after an already pending catalog request finishes', async () => {
    let resolvePrevious!: (value: unknown) => void
    request.mockReturnValueOnce(new Promise(resolve => resolvePrevious = resolve))
    const defaults = { model: 'opencode/inherited-model', reasoning: '', speed: '' }
    const response = {
      models: [],
      checkedAt: 1,
      stale: false,
      error: '',
    }
    request.mockResolvedValueOnce({ ...response, checkedAt: 2, nativeDefaults: defaults })
    const { modelCatalogs } = await import('../src/models')
    const previous = modelCatalogs.opencode.load()
    const refreshed = modelCatalogs.opencode.refresh()
    expect(modelCatalogs.opencode.catalog.loaded).toBe(false)
    expect(request).toHaveBeenCalledTimes(1)
    resolvePrevious(response)
    await previous
    await refreshed
    expect(request).toHaveBeenCalledTimes(2)
    expect(modelCatalogs.opencode.catalog.nativeDefaults).toEqual(defaults)
  })

  it('clears inherited preferences when a refresh omits the reset installation defaults', async () => {
    const defaults = { model: 'opencode/inherited-model', reasoning: '', speed: '' }
    const response = {
      models: [{ model: 'opencode/inherited-model' }],
      checkedAt: 1,
      stale: false,
      error: '',
    }
    request.mockResolvedValueOnce({ ...response, nativeDefaults: defaults })
    const { modelCatalogs } = await import('../src/models')
    await modelCatalogs.opencode.load()
    expect(modelCatalogs.opencode.catalog.nativeDefaults).toEqual(defaults)
    request.mockResolvedValueOnce({ ...response, checkedAt: 2 })
    await modelCatalogs.opencode.load()
    expect(modelCatalogs.opencode.catalog.nativeDefaults).toBeUndefined()
  })

  it('keeps whether an agent is connected across reloads and forgets it for an older installation', async () => {
    const response = {
      models: [],
      checkedAt: null,
      stale: false,
      error: 'Connect an OpenCode model-provider API key in Connections to choose a model.',
    }
    request.mockResolvedValueOnce({ ...response, connected: false })
    let { modelCatalogs } = await import('../src/models')
    expect(modelCatalogs.opencode.catalog.connected).toBeUndefined()
    await modelCatalogs.opencode.load()
    expect(modelCatalogs.opencode.catalog.connected).toBe(false)

    vi.resetModules()
    ;({ modelCatalogs } = await import('../src/models'))
    expect(modelCatalogs.opencode.catalog.connected).toBe(false)
    request.mockResolvedValueOnce(response)
    await modelCatalogs.opencode.load()
    expect(modelCatalogs.opencode.catalog.connected).toBeUndefined()
  })

  it('loads the Antigravity catalog with its reasoning levels from its own endpoint', async () => {
    const gemini = {
      model: 'gemini-4-neon',
      displayName: 'Gemini 4 Neon',
      description: 'Via Gemini API, Google Cloud',
      isDefault: true,
      defaultReasoningEffort: 'high',
      supportedReasoningEfforts: [{ reasoningEffort: 'low', description: '' }, { reasoningEffort: 'high', description: '' }],
    }
    request.mockResolvedValueOnce({
      models: [gemini],
      checkedAt: 1,
      stale: false,
      error: '',
    })
    const { modelCatalogs } = await import('../src/models')
    await modelCatalogs.antigravity.load()
    expect(request).toHaveBeenCalledWith('/antigravity/models', { method: 'GET' })
    expect(modelCatalogs.antigravity.catalog.models).toEqual([gemini])
    expect(localStorage.getItem('fixture-installation:cairn-models:antigravity')).toContain('gemini-4-neon')
  })

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

  it('keeps and persists inventory when a connection is confirmed but model discovery is stale', async () => {
    const saved = [{ model: 'opencode/saved-model' }]
    localStorage.setItem('fixture-installation:cairn-models:opencode', JSON.stringify({ models: saved }))
    const { modelCatalogs } = await import('../src/models')
    const catalog = modelCatalogs.opencode.catalog
    request.mockResolvedValueOnce({
      connected: true,
      models: [],
      stale: true,
      error: 'Inventory unavailable',
    })
    await modelCatalogs.opencode.load()
    expect(catalog).toMatchObject({
      connected: true,
      models: saved,
      stale: true,
      error: 'Inventory unavailable',
    })
    expect(localStorage.getItem('fixture-installation:cairn-models:opencode')).toContain('opencode/saved-model')

    request.mockResolvedValueOnce({
      connected: true,
      models: [],
      stale: false,
      error: '',
    })
    await modelCatalogs.opencode.load()
    expect(catalog.models).toEqual([])
  })
})
