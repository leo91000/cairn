import { main } from '../../../scripts/ci-backend-durable.mjs'
import { foregroundAction } from '../../../scripts/ci-backend-trace.mjs'

void foregroundAction(main)
