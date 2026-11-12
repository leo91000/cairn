import { readFileSync } from 'node:fs'
import vm from 'node:vm'
import ts from 'typescript'
import { expect, it, vi } from 'vitest'
import * as vue from 'vue'
import { compileScript, parse } from 'vue/compiler-sfc'
import * as agentAccess from '../src/agent-access'
import * as agentUse from '../src/agent-use'
import * as pluginGrants from '../src/plugin-grants'

// Execute the actual page setup and save path without mounting its unrelated presentation.
// API calls are observed at the generated-client boundary; draft/computed logic stays real.
function pageSetup() {
  const source = readFileSync(new URL('../src/views/AgentPage.vue', import.meta.url), 'utf8')
  const script = compileScript(parse(source).descriptor, { id: 'agent-page-test' }).content
  const code = ts.transpileModule(script, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText
  const original = {
    ...agentAccess.newAgentDraft(),
    id: 'agent',
    name: 'Agent',
    revision: 'r1',
    createdAt: 0,
  }
  const profileRequests: any[] = []
  const grantRequests: any[] = []
  const state = {
    projects: [],
    skills: [],
    mcps: [],
    workspaceLoaded: true,
    installationRole: 'owner',
  }
  const imports: Record<string, any> = {
    'vue': {
      ...vue,
      onMounted() {},
      onBeforeUnmount() {},
      watch() {},
    },
    'vue-router': { useRoute: () => ({ params: { id: original.id }, query: {} }), useRouter: () => ({}), onBeforeRouteLeave() {} },
    '../agent-access': agentAccess,
    '../plugin-grants': pluginGrants,
    '../agent-use': agentUse,
    // The owner's member list is not part of these save paths.
    '../agent-people': {
      useAgentPeople: () => ({
        rows: vue.ref([{ known: true, allowed: true, email: 'ada@example.test' }]),
        count: vue.ref(0),
        loaded: vue.ref(true),
        error: vue.ref(''),
        outdated: vue.ref(false),
        load: async () => {},
      }),
    },
    '../api': {
      api: {},
      state,
      ApiError: class extends Error {},
      refresh: async () => {},
      notify() {},
    },
    '../resourceSave': { useResourceSave: () => ({}) },
    '../ui': { buttonVariants: {}, buttonSizes: {} },
  }
  const exports: any = {}
  vm.runInNewContext(code, {
    exports,
    structuredClone,
    require: (path: string) => {
      if (imports[path])
        return imports[path]
      if (path.endsWith('/replaceAgent')) {
        return {
          replaceAgent: vi.fn(async (_api, body) => {
            profileRequests.push(body)
            return JSON.parse(JSON.stringify({ ...original, ...body.settings, revision: 'r2' }))
          }),
        }
      }

      if (path.endsWith('/previewAgent'))
        return { previewAgent: async () => ({ runs: { queued: 0, running: 0 }, delegations: [] }) }

      if (path.endsWith('/replaceAgentPluginGrants')) {
        return {
          replaceAgentPluginGrants: vi.fn(async (_api, body) => {
            grantRequests.push(body)
            return JSON.parse(JSON.stringify({ ...body, revision: 'r3' }))
          }),
        }
      }

      return {}
    },
  })
  const page = exports.default.setup({}, { expose() {} })
  page.agent.value = original
  page.saved.value = agentAccess.draftOf(original as any)
  page.draft.value = agentAccess.draftOf(original as any)
  page.loaded.value = true
  return { page, profileRequests, grantRequests }
}

it.each([false, true])('grant-only save sends profile review only for an explicit reapproval (%s)', async (checked) => {
  const { page, profileRequests, grantRequests } = pageSetup()
  page.grants.value = { grants: [{ instanceId: 'one', access: { type: 'full' } }], runCredentialInstances: [] }
  await page.commit(checked ? ['previously-suspended-bot'] : [])
  expect(profileRequests).toHaveLength(checked ? 1 : 0)
  if (checked) {
    expect(profileRequests[0]).toMatchObject({
      agentId: 'agent',
      revision: 'r1',
      settings: { plugins: [] },
      review: { reapproveDelegations: ['previously-suspended-bot'] },
    })
  }

  expect(page.serverError.value).toBe('')
  expect(grantRequests).toHaveLength(1)
  expect(grantRequests[0].revision).toBe(checked ? 'r2' : 'r1')
})

it('reviews a grant-only addition with the people it reaches', async () => {
  const { page, profileRequests, grantRequests } = pageSetup()
  page.grants.value = { grants: [{ instanceId: 'one', access: { type: 'full' } }], runCredentialInstances: [] }
  await page.requestSave()
  expect(page.review.value.lines.map((line: any) => line.kind)).toEqual(['secret', 'audience'])
  expect(page.review.value.lines[1].text).toContain('Installation owner, ada@example.test')
  expect(profileRequests).toHaveLength(0)
  expect(grantRequests).toHaveLength(0)
})

it('warns for selected disabled/unavailable instances and drops warnings after local revocation', () => {
  const { page } = pageSetup()
  page.catalog.value = {
    packages: [{
      id: 'notes',
      status: 'active',
      manifest: {
        id: 'notes',
        name: 'Notes',
        agentAccess: {},
        instructions: 'Read notes.',
      },
    }],
    connections: [
      {
        id: 'disabled',
        pluginId: 'notes',
        name: 'Disabled notes',
        enabled: false,
        status: { state: 'ready' },
      },
      {
        id: 'unavailable',
        pluginId: 'missing',
        name: 'Missing notes',
        enabled: true,
        status: { state: 'ready' },
      },
    ],
  }
  page.grants.value = {
    grants: ['disabled', 'unavailable'].map(instanceId => ({ instanceId, access: { type: 'selected', contributions: [{ kind: 'instructions', name: '', resources: [] }] } })),
    runCredentialInstances: [],
  }
  expect(page.issues.value.filter((issue: any) => issue.section === 'plugins').map((issue: any) => issue.text)).toEqual([
    expect.stringContaining('is disabled'),
    expect.stringContaining('is unavailable'),
  ])
  // The profile compatibility projection is full-only and must stay out of presentation logic.
  expect(page.body().settings.plugins).toEqual([])
  page.draft.value.plugins = ['disabled']
  page.grants.value = { grants: [], runCredentialInstances: [] }
  expect(page.issues.value.filter((issue: any) => issue.section === 'plugins')).toEqual([])
  expect(page.body().settings.plugins).toEqual(['disabled'])
})
