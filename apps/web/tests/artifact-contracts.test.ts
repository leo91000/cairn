import type { Deliverable } from '../../../packages/contracts/generated/typescript/Deliverable'
import type { SetRunArtifactVisibilityInput } from '../../../packages/contracts/generated/typescript/SetRunArtifactVisibilityInput'
import Ajv from 'ajv'
import addFormats from 'ajv-formats'
import { expect, it } from 'vitest'
import metadataSchema from '../../../packages/contracts/generated/Deliverable.json'
import visibilitySchema from '../../../packages/contracts/generated/SetRunArtifactVisibilityInput.json'
import corpus from '../../../tests/fixtures/artifact-contracts.json'

it('keeps artifact metadata and string visibility consistent with Rust and Kotlin', () => {
  const ajv = new Ajv({ strict: false })
  addFormats(ajv)
  const visibility = ajv.compile<SetRunArtifactVisibilityInput>(visibilitySchema)
  for (const value of corpus.visibilityInputs)
    expect(visibility(value), JSON.stringify(visibility.errors)).toBe(true)
  for (const value of corpus.invalidVisibilityInputs)
    expect(visibility(value)).toBe(false)
  const metadata = ajv.compile<Deliverable>(metadataSchema)
  expect(metadata(corpus.metadata), JSON.stringify(metadata.errors)).toBe(true)
})
