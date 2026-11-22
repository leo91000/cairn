import { describe, expect, it } from 'vitest'
import { providers } from '../../../packages/contracts/accounts'
import {
  antigravityTermsWarning,
  brands,
  connectsWithKey,
  credentialsProject,
  googleAccount,
  googleCloudKey,
  needsTermsConfirmation,
  reconnectLabel,
} from '../src/accounts'
import { BrandGemini } from '../src/icons'

const serviceAccount = { type: 'service_account', project_id: 'from-file', client_email: 'agent@from-file.iam.gserviceaccount.com' }
const authorizedUser = { type: 'authorized_user', client_id: 'id', refresh_token: 'token' }

describe('antigravity coding agent', () => {
  it('is listed with the Gemini mark, user-ordered accounts and plan turns', () => {
    expect(providers.antigravity).toMatchObject({
      label: 'Antigravity',
      vendor: 'Google',
      connection: 'choice',
      requiresModel: false,
      ordered: true,
      planTurns: true,
    })
    expect(brands.antigravity.icon).toBe(BrandGemini)
  })

  it('connects with a key or credentials unless the Google account sign-in is chosen', () => {
    expect(connectsWithKey('antigravity', 'gemini')).toBe(true)
    expect(connectsWithKey('antigravity', 'google-cloud')).toBe(true)
    expect(connectsWithKey('antigravity', googleAccount)).toBe(false)
    expect(connectsWithKey('antigravity', null)).toBe(false)
    expect(connectsWithKey('opencode', 'opencode-go')).toBe(true)
    expect(connectsWithKey('claude', null)).toBe(false)
  })

  it('requires the terms confirmation only before a Google account sign-in', () => {
    expect(needsTermsConfirmation('antigravity', googleAccount)).toBe(true)
    expect(needsTermsConfirmation('antigravity', 'gemini')).toBe(false)
    expect(needsTermsConfirmation('antigravity', 'google-cloud')).toBe(false)
    expect(needsTermsConfirmation('claude', googleAccount)).toBe(false)
    expect(antigravityTermsWarning).toContain('Google may suspend the account')
    expect(antigravityTermsWarning).toContain('Prefer a Gemini API key or Google Cloud credentials')
  })

  it('reconnects each account kind its own way', () => {
    expect(reconnectLabel({ provider: 'antigravity', modelProvider: 'gemini', state: 'ready' })).toBe('Replace API key')
    expect(reconnectLabel({ provider: 'antigravity', modelProvider: 'google-cloud', state: 'ready' })).toBe('Replace credentials')
    expect(reconnectLabel({ provider: 'antigravity', modelProvider: googleAccount, state: 'error' })).toBe('Reconnect')
    expect(reconnectLabel({ provider: 'antigravity', modelProvider: googleAccount, state: 'pending' })).toBe('Sign in')
    expect(reconnectLabel({ provider: 'opencode', modelProvider: 'opencode', state: 'ready' })).toBe('Replace API key')
    expect(reconnectLabel({ provider: 'codex', modelProvider: null, state: 'ready' })).toBe('Reconnect')
  })
})

describe('google Cloud credentials', () => {
  it('composes the credentials file, project and region into one key', () => {
    const key = googleCloudKey({ credentials: JSON.stringify(serviceAccount), project: ' my-project ', location: ' europe-west4 ' })
    expect(typeof key).toBe('string')
    expect(JSON.parse(key)).toEqual({ credentials: serviceAccount, project: 'my-project', location: 'europe-west4' })
  })

  it('leaves out an empty project and region so the file and global apply', () => {
    expect(JSON.parse(googleCloudKey({ credentials: JSON.stringify(serviceAccount), project: '', location: '' })))
      .toEqual({ credentials: serviceAccount })
    expect(credentialsProject(JSON.stringify({ quota_project_id: 'quota' }))).toBe('quota')
    expect(credentialsProject('not json')).toBe('')
  })

  it('asks for a project the credentials file does not name', () => {
    expect(() => googleCloudKey({ credentials: JSON.stringify(authorizedUser), project: '', location: '' }))
      .toThrow('Enter the Google Cloud project')
    expect(JSON.parse(googleCloudKey({ credentials: JSON.stringify(authorizedUser), project: 'p', location: '' })))
      .toEqual({ credentials: authorizedUser, project: 'p' })
  })

  it('rejects text that is not a JSON object', () => {
    for (const credentials of ['', 'not json', '[]', '"text"', 'null'])
      expect(() => googleCloudKey({ credentials, project: 'p', location: '' })).toThrow('Paste the credentials JSON file')
  })
})
