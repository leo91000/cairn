import type { QualificationFixture } from '../../../packages/contracts/generated/typescript/QualificationFixture'
import Ajv from 'ajv'
import addFormats from 'ajv-formats'
import { describe, expect, it } from 'vitest'
import expectedWire from '../../../packages/contracts/generated/qualification-wire.json'
import schema from '../../../packages/contracts/generated/QualificationFixture.json'
import { resumeRun, resumeRunId } from '../../../packages/contracts/generated/resume-run'
import { resumeChat } from '../../../packages/contracts/generated/resumeChat'
import corpus from '../../../tests/fixtures/resume-contracts.json'

const ajv = new Ajv({ useDefaults: true, strict: false })
addFormats(ajv)
const validate = ajv.compile<QualificationFixture>(schema)

describe('generated resume contracts', () => {
  it('decodes the shared object, tagged variants, defaults, nulls, IDs, list/map, integers and errors', () => {
    for (const value of corpus.valid) {
      const decoded: unknown = JSON.parse(JSON.stringify(value))
      expect(validate(decoded), JSON.stringify(validate.errors)).toBe(true)
      if (!validate(decoded))
        throw new Error('Invalid fixture')
      expect(typeof decoded.enabled).toBe('boolean')
      expect(Number.isSafeInteger(decoded.count)).toBe(true)
      expect(decoded.names.run).toBe('mission')
      if (decoded.result.kind === 'rejected')
        expect(decoded.result.response.error).toBe('Run already active')
      else
        expect(decoded.result.runId).toBe(decoded.id)
    }

    for (const value of [...corpus.invalid, ...corpus.serverInvalid])
      expect(validate(structuredClone(value)), JSON.stringify(value)).toBe(false)
  })

  it('preserves the Rust wire shape, including required null and omitted optional values', () => {
    for (const expected of expectedWire) {
      const decoded: unknown = JSON.parse(JSON.stringify(expected))
      expect(validate(decoded), JSON.stringify(validate.errors)).toBe(true)
      expect(JSON.parse(JSON.stringify(decoded))).toEqual(expected)
      expect(Object.hasOwn(decoded as object, 'nullable')).toBe(true)
    }

    const missingNullable: Record<string, unknown> = { ...expectedWire[0] }
    delete missingNullable.nullable
    expect(validate(missingNullable)).toBe(false)
  })

  it('uses the generated capability call without creating a run', async () => {
    const calls: unknown[] = []
    const request = async <T>(path: string, options: RequestInit): Promise<T> => {
      calls.push([path, options])
      return { runId: corpus.valid[0].id } as T
    }

    expect(resumeRunId).toBe('resume_run')
    expect(await resumeRun(request, { runId: corpus.valid[0].id })).toEqual({ runId: corpus.valid[0].id })
    expect(calls).toEqual([[
      `/runs/${corpus.valid[0].id}/resume`,
      { method: 'POST', body: JSON.stringify({ runId: corpus.valid[0].id }) },
    ]])
  })

  it('sends a guarded same-chat selection through the generated client', async () => {
    const calls: unknown[] = []
    const request = async <T>(path: string, options: RequestInit): Promise<T> => {
      calls.push([path, options])
      return { id: corpus.valid[0].id } as T
    }

    const input = {
      chatId: corpus.valid[0].id,
      runId: corpus.valid[0].id,
      turnId: 'turn-current',
      selection: {
        model: '',
        preserveModel: true,
        reasoning: '',
        speed: 'standard',
      },
    }

    await resumeChat(request, input)
    expect(calls).toEqual([[
      `/chats/${corpus.valid[0].id}/resume`,
      { method: 'POST', body: JSON.stringify(input) },
    ]])
  })
})
