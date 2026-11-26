import { main } from '../../../scripts/ci/ci-backend-durable.mjs'
import { foregroundAction } from '../../../scripts/ci/ci-backend-trace.mjs'

void foregroundAction(main)
