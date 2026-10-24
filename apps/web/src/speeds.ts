import type { Provider } from '../../../packages/contracts/accounts'
import type { CodingAgentModel, ServiceTier } from '../../../packages/contracts/models'
import { STANDARD_SPEED } from '../../../packages/contracts/models'

// A speed is independent of the model and the reasoning level: it is kept when either
// changes. Only Codex lists speeds, per model and account, in its catalog.

/** The speed a run uses: the chosen one, else the inherited one, else the model's own default. */
export function effectiveSpeed(speed: string, inherited: string, model?: CodingAgentModel) {
  return speed || inherited || model?.defaultServiceTier || STANDARD_SPEED
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

export function speedTier(model: CodingAgentModel | undefined, speed: string): ServiceTier | undefined {
  return model?.serviceTiers?.find(tier => tier.id === speed)
}
