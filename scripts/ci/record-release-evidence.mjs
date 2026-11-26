import { mkdirSync, writeFileSync } from 'node:fs'
import process from 'node:process'

mkdirSync('release-evidence', { recursive: true })
writeFileSync('release-evidence/image.json', JSON.stringify({
  schema: 3,
  beaconDigest: process.env.BEACON_IMAGE_DIGEST,
  repository: process.env.GITHUB_REPOSITORY.toLowerCase(),
  tree: process.env.GIT_TREE,
  commit: process.env.GITHUB_SHA,
  digest: process.env.IMAGE_DIGEST,
  runId: Number(process.env.GITHUB_RUN_ID),
  tools: {
    codex: process.env.CODEX_VERSION,
    gh: process.env.GH_VERSION,
    claude: process.env.CLAUDE_VERSION,
    opencode: process.env.OPENCODE_VERSION,
    antigravity: process.env.ANTIGRAVITY_VERSION,
  },
  served: process.env.SERVED_FOUND === 'true'
    ? { claude: process.env.SERVED_CLAUDE, opencode: process.env.SERVED_OPENCODE }
    : null,
}))
