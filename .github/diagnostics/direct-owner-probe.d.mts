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

export interface DirectOwnerSseSdkProjection {
  hostCollectorVersion: 'DIRECT_OWNER_HOST_V2_PUBLIC_SDK_FIRST_SSE'
  correlationScope: 'PINNED_SOURCE_CONDITIONAL'
  sameDocument: true | null
  sameGrantConnection: true | null
  sameSource: true | null
  sdkCandidateRequestCount: number
  nativeFirstRequestExposureGuaranteed: false
  sdkNativeDocumentIdentityExposed: false
  captureClosed: boolean
  closureKnown: boolean
  metadataFailureCount: number
  orphanTerminalCount: number
  sdkResponseEventObserved: boolean | null
  responseObserved: boolean | null
  responseStatus: number | null
  requestFailedObserved: boolean | null
  requestFinishedObserved: boolean | null
  sdkTerminalEvent: 'requestfailed' | 'requestfinished' | 'UNKNOWN'
  sdkErrorTextCategory: 'NOT_OBSERVED' | 'SDK_FAILURE_TEXT_MISSING' | 'SDK_CANCEL_REPORTED' | 'SDK_ACCESS_CONTROL_REPORTED' | 'SDK_OTHER_FAILURE' | 'UNKNOWN'
  sdkCancellationReport: 'KNOWN_FROM_SDK_TEXT' | 'UNKNOWN'
  completion: 'SDK_FAILED' | 'SDK_FINISHED' | 'UNKNOWN'
  receiptStatus: 'SDK_TERMINAL_SOURCE_CONDITIONAL' | 'UNKNOWN'
  nativeStructuredReason: 'NOT_EXPOSED_BY_PUBLIC_API'
  nativeDocumentLoaderCancellation: 'UNKNOWN'
  nativeCauseConclusive: false
  productQualified: false
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
  finish: (closure: DirectOwnerClosure) => { privateEvidence: unknown, projection: DirectOwnerProjection & { sdkSse: DirectOwnerSseSdkProjection } }
  dispose: () => boolean
}
