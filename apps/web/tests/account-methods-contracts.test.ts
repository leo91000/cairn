import type { ListAccountMethodsOutput } from '../../../packages/contracts/generated/typescript/ListAccountMethodsOutput'
import Ajv from 'ajv'
import { expect, it } from 'vitest'
import { listAccountMethods } from '../../../packages/contracts/generated/list-account-methods'
import outputSchema from '../../../packages/contracts/generated/ListAccountMethodsOutput.json'
import corpus from '../../../tests/fixtures/account-methods-contracts.json'

it('qualifies account method responses against the shared contract', () => {
  const ajv = new Ajv({ strict: false })
  const output = ajv.compile<ListAccountMethodsOutput>(outputSchema)
  for (const value of corpus.valid)
    expect(output(value), JSON.stringify(output.errors)).toBe(true)
  for (const value of corpus.invalid)
    expect(output(value), JSON.stringify(value)).toBe(false)
})

it('uses the account transport without accepting an account or installation identity', async () => {
  const routes: string[] = []
  const request = async <T>(route: string): Promise<T> => {
    routes.push(route)
    return corpus.valid[0] as T
  }

  expect(await listAccountMethods(request)).toEqual(corpus.valid[0])
  expect(routes).toEqual(['methods'])
})
