import Ajv from 'ajv'
import addFormats from 'ajv-formats'
import { expect, it } from 'vitest'
import settingsSchema from '../../../packages/contracts/generated/AccountSettings.json'
import accountSchema from '../../../packages/contracts/generated/CodingAgentAccount.json'
import corpus from '../../../tests/fixtures/coding-agent-account-contracts.json'

it('qualifies account usage decimals, unknown usage and bounded parallelism', () => {
  const ajv = new Ajv({ strict: false, useDefaults: true })
  addFormats(ajv)
  const account = ajv.compile(accountSchema)
  const settings = ajv.compile(settingsSchema)
  for (const value of corpus.accounts)
    expect(account(structuredClone(value)), JSON.stringify(account.errors)).toBe(true)
  for (const value of corpus.settings)
    expect(settings(value)).toBe(true)
  for (const value of corpus.invalidSettings)
    expect(settings(value)).toBe(false)
})
