import type { Page } from '@playwright/test'

export interface DirectOwnerInstallationScope {
  installationId: string
  installationSlug: string
}

export interface DirectOwnerClosure {
  pageClosed: boolean
  gateClosed: boolean
  cleanupFailureCount: number
}

export interface DirectOwnerProjection {
  diagnosticOnly: true
  productQualified: false
  ownershipEvidenceScope: 'PINNED_SOURCE_CONDITIONAL'
  nativeOperationEvidenceScope: 'PINNED_SOURCE_CONDITIONAL'
  universalNativeOperationEquivalence: false
  outOfDomainOperationEquivalence: 'NOT_CLAIMED'
  universalRuntimeAdmission: false
  runtimeProducerReceipt: false
  proxyDetection: 'NOT_PROVIDED'
  installationScopeBasis: string
  nativeGenerationPublicationCorrelation: false
  ownerReactionStateKnown: boolean
  sourceCloseProvenanceKnown: boolean
  firstErrorPairReproduced: boolean
  closureKnown: boolean
  pageClosed: boolean | null
  gateClosed: boolean | null
  cleanupFailureCount: number | null
  receiptStatus: 'SOURCE_CONDITIONAL_PARTIAL' | 'UNKNOWN'
  nativeOrderingAgainstBothPageErrors: 'UNKNOWN'
}

export function observeDirectOwners(scope: DirectOwnerInstallationScope): void
export function projectDirectOwners(journal: unknown, pageErrors: unknown, observerFailures?: unknown, closure?: DirectOwnerClosure): DirectOwnerProjection
export function createDirectOwnerObserver(page: Page, scope: DirectOwnerInstallationScope): {
  install: () => Promise<void>
  accept: (snapshot: unknown) => void
  finish: (closure: DirectOwnerClosure) => { privateEvidence: unknown, projection: DirectOwnerProjection }
  dispose: () => boolean
}
