import type { Page } from '@playwright/test'

export interface DirectOwnerClosure {
  pageClosed: boolean
  gateClosed: boolean
  cleanupFailureCount: number
}

export interface DirectOwnerProjection {
  diagnosticOnly: true
  productQualified: false
  nativeGenerationPublicationCorrelation: false
  ownerReactionStateKnown: boolean
  sourceCloseProvenanceKnown: boolean
  firstErrorPairReproduced: boolean
  closureKnown: boolean
  pageClosed: boolean | null
  gateClosed: boolean | null
  cleanupFailureCount: number | null
  receiptStatus: 'PARTIAL' | 'UNKNOWN'
  nativeOrderingAgainstBothPageErrors: 'UNKNOWN'
}

export function observeDirectOwners(): void
export function projectDirectOwners(journal: unknown, pageErrors: unknown, observerFailures?: unknown, closure?: DirectOwnerClosure): DirectOwnerProjection
export function createDirectOwnerObserver(page: Page): {
  install: () => Promise<void>
  accept: (snapshot: unknown) => void
  finish: (closure: DirectOwnerClosure) => { privateEvidence: unknown, projection: DirectOwnerProjection }
  dispose: () => boolean
}
