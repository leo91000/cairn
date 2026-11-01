import { Buffer } from 'node:buffer'
import { execFileSync } from 'node:child_process'
import {
  afterEach,
  describe,
  expect,
  it,
  vi,
} from 'vitest'
import { executeBeaconSql } from '../../tests/e2e/beacon-relay-fixture'

vi.mock('node:child_process', async (original) => {
  const actual = await original<typeof import('node:child_process')>()
  return { ...actual, execFileSync: vi.fn(() => Buffer.alloc(0)) }
})

afterEach(() => {
  vi.clearAllMocks()
  vi.unstubAllEnvs()
})

describe('beacon SQL fixture transport', () => {
  it('sends the same connection fields and raw batch only on stdin and remains synchronous', () => {
    vi.stubEnv('PGPASSWORD', 'ambient_password')
    vi.stubEnv('PGOPTIONS', '-c search_path=ambient_schema')
    vi.stubEnv('CAIRN_BEACON_TEST_DATABASE_URL', 'postgres://ambient:password@private.invalid/db')
    vi.stubEnv('PGSSLMODE', 'require')
    const database = new URL('postgres://fixture%40user:fixture%40password@postgres:5432/fixture_db?options=-c%20search_path%3Dfixture_schema&sslmode=disable')
    const sql = 'CREATE SCHEMA fixture_schema; INSERT INTO fixture_schema.entries VALUES (1);'
    expect(executeBeaconSql(database, sql)).toBeUndefined()
    const [binary, args, options] = vi.mocked(execFileSync).mock.calls[0]
    expect(binary).toBe('target/debug/examples/beacon_sql_fixture')
    expect(args).toEqual([])
    expect(options?.stdio).toBe('pipe')
    expect(options?.env?.PGPASSWORD).toBeUndefined()
    expect(options?.env?.PGOPTIONS).toBeUndefined()
    expect(options?.env?.CAIRN_BEACON_TEST_DATABASE_URL).toBeUndefined()
    expect(options?.env?.PGSSLMODE).toBe('require')
    expect(typeof options?.input).toBe('string')
    const request = JSON.parse(options?.input as string)
    const connection = new URL(request.database_url)
    expect(connection.username).toBe(database.username)
    expect(connection.password).toBe(database.password)
    expect(connection.hostname).toBe(database.hostname)
    expect(connection.port).toBe(database.port)
    expect(connection.pathname).toBe(database.pathname)
    expect([...connection.searchParams]).toEqual([['options', '-c search_path=fixture_schema']])
    expect(request.sql).toBe(sql)
    expect(database.searchParams.get('sslmode')).toBe('disable')
  })

  it('propagates the exact child error instead of continuing setup', () => {
    const failure = Object.assign(new Error('synthetic fixture failure'), { status: 3 })
    vi.mocked(execFileSync).mockImplementationOnce(() => {
      throw failure
    })
    expect(() => executeBeaconSql(new URL('postgres://fixture:fixture@postgres/fixture_db'), 'SELECT 1')).toThrow(failure)
  })
})
