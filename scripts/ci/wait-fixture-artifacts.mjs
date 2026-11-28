import { execFileSync } from 'node:child_process'
import process from 'node:process'
import { waitForArtifact } from '../android/android-device-tests.mjs'

const gh = args => execFileSync('gh', args, { encoding: 'utf8' })
for (const artifact of ['browser-backend', 'browser-build']) {
  await waitForArtifact({
    repository: process.env.GITHUB_REPOSITORY,
    runId: process.env.GITHUB_RUN_ID,
    attempt: process.env.GITHUB_RUN_ATTEMPT,
    artifact,
    job: 'build',
  }, { gh })
}
