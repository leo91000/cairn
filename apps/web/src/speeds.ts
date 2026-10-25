import type { Provider } from '../../../packages/contracts/accounts'
import type { CodingAgentModel, ServiceTier } from '../../../packages/contracts/models'
import { speedLabel, STANDARD_SPEED } from '../../../packages/contracts/models'

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
 * listed or from an older Codex, cannot tell: the run then leaves the check to Codex.
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

/**
 * A queued or saved speed as it runs: one its model lacks runs at Standard. A model missing from
 * the catalog leaves the check to Codex, as the run does.
 */
export function speedNote(speed: string, model: string, models: CodingAgentModel[]) {
  if (!speed)
    return ''
  const label = speedLabel(speed, models.flatMap(item => item.serviceTiers ?? []))
  return offersSpeed(models.find(item => item.model === model), speed) ? `${label} speed` : `Standard speed (${label} not offered)`
}

export function speedTier(model: CodingAgentModel | undefined, speed: string): ServiceTier | undefined {
  return model?.serviceTiers?.find(tier => tier.id === speed)
}
