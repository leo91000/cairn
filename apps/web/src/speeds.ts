import type { Provider } from '../../../packages/contracts/accounts'
import type { CodingAgentModel, ServiceTier } from '../../../packages/contracts/models'
import { speedLabel, STANDARD_SPEED } from '../../../packages/contracts/models'

export type ResumeReasoningIntent
  = | { kind: 'untouched' }
    | { kind: 'model-default' }
    | { kind: 'effort', value: string }

// A speed is independent of the model and the reasoning level: it is kept when either
// changes. Only Codex lists speeds, per model and account, in its catalog.

/**
 * The speed a run uses: the chosen one, else the inherited one, else Standard. Codex's app
 * server does not apply a model's `defaultServiceTier`, so Cairn never claims it.
 */
export function effectiveSpeed(speed: string, inherited: string) {
  return speed || inherited || STANDARD_SPEED
}

/**
 * Whether the model offers `speed`. A catalog without speeds, cached before they were
 * listed, cannot tell: the turn then checks with Codex.
 */
export function offersSpeed(model: CodingAgentModel | undefined, speed: string) {
  if (!speed || speed === STANDARD_SPEED || !model?.serviceTiers)
    return true
  return model.serviceTiers.some(tier => tier.id === speed)
}

/** The speed section shows for Codex models that list speeds, or to keep a saved choice. */
export function showsSpeed(provider: Provider, model: CodingAgentModel | undefined, speed: string) {
  return provider === 'codex' && (!!model?.serviceTiers?.length || !!speed)
}

/** The speed shown by the picker after applying its provider and selected-model rules. */
export function pickerAppliedSpeed(
  provider: Provider,
  model: CodingAgentModel | undefined,
  speed: string,
  inherited: string,
) {
  const current = effectiveSpeed(speed, inherited)
  return showsSpeed(provider, model, speed || inherited) && offersSpeed(model, current)
    ? current
    : STANDARD_SPEED
}

/** Only Codex accepts a speed override; keep its picker-applied value, including Standard. */
export function resumeSelectionSpeed(
  provider: Provider,
  model: CodingAgentModel | undefined,
  speed: string,
  inherited: string,
) {
  const applied = pickerAppliedSpeed(provider, model, speed, inherited)
  return provider === 'codex' ? applied : ''
}

/** Whether resuming would change the selected request from the saved request. */
export function needsResumeSelection(
  selectedModel: string,
  selectedReasoning: string,
  selectedSpeed: string,
  savedModel: string,
  savedReasoning: string,
  savedSpeed: string,
) {
  return selectedModel !== savedModel
    || selectedReasoning !== savedReasoning
    || selectedSpeed !== effectiveSpeed('', savedSpeed)
}

/** Resolve the picker choice without confusing an untouched blank with an explicit default. */
export function resumeSelectionReasoning(
  intent: ResumeReasoningIntent,
  reasoning: string,
  savedReasoning: string,
  modelChanged: boolean,
  modelDefault: string,
) {
  if (intent.kind === 'effort')
    return intent.value
  if (intent.kind === 'model-default')
    return modelDefault
  if (reasoning)
    return reasoning
  if (modelChanged)
    return modelDefault
  return savedReasoning
}

/**
 * A queued or saved faster speed is a request, which each turn reports on, since Codex may answer
 * with its built-in list and an account's plan decides; one the model's catalog lacks says so. An
 * empty model is the catalog's default.
 */
export function speedNote(speed: string, model: string, models: CodingAgentModel[]) {
  if (!speed)
    return ''
  const label = speedLabel(speed, models.flatMap(item => item.serviceTiers ?? []))
  if (speed === STANDARD_SPEED)
    return `${label} speed`
  const row = models.find(item => model ? item.model === model : item.isDefault)
  if (row?.serviceTiers && !offersSpeed(row, speed))
    return `${label} speed requested (not listed for this model)`
  return `${label} speed requested`
}

export function speedTier(model: CodingAgentModel | undefined, speed: string): ServiceTier | undefined {
  return model?.serviceTiers?.find(tier => tier.id === speed)
}
