import { mkdir, writeFile } from 'node:fs/promises'
import process from 'node:process'
import { instrument, junitXml, parseInstrumentation } from './android-device-tests.mjs'

export const stabilityTests = [
  'build.cairn.app.ui.SkillMentionDeviceTest#emptySkillsExplainsTheMissingSuggestionsAndCanBeDismissed',
  'build.cairn.app.ui.ChatSwipeDeviceTest#notificationReopensItsRequestedChatAfterSwipingAwayFromIt',
  'build.cairn.app.ui.HistoryFollowDeviceTest#compactConversationAndKeyboard',
]

// A fixed sample count, never a retry-until-green loop. Normal CI still runs
// the complete selected shard; this opt-in measurement adds the ticket journeys.
export async function measureComposeStability(count, output, {
  run = instrument,
  save = writeFile,
  prepare = mkdir,
  log = console.log,
} = {}) {
  if (!/^(?:[1-9]|1\d|20)$/.test(String(count)) || !output)
    throw new Error('Usage: android-compose-stability.mjs <1–20 trials> <output directory>')
  await prepare(output, { recursive: true })
  const trials = []
  let ok = false
  try {
    for (let index = 1; index <= Number(count); index++) {
      const lines = await run(['-e', 'class', stabilityTests.join(',')])
      const report = parseInstrumentation(lines)
      const actual = report.tests.map(test => `${test.className}#${test.name}`).sort()
      const complete = JSON.stringify(actual) === JSON.stringify([...stabilityTests].sort())
      const passed = report.tests.every(test => test.status === 'passed')
      trials.push({ index, ...report, ok: report.ok && complete && passed })
      await save(`${output}/trial-${index}.txt`, `${lines.map(line => line.text).join('\n')}\n`)
      await save(`${output}/TEST-compose-trial-${index}.xml`, junitXml(report.tests, `compose-trial-${index}`))
      log(`Compose stability trial ${index}/${count}: ${trials.at(-1).ok ? 'passed' : 'failed'}`)
      if (!trials.at(-1).ok)
        throw new Error(`Compose stability trial ${index}/${count} failed or did not run all three journeys. No retry. ${report.problems.join('; ')}`)
    }

    ok = true
    return trials
  }
  finally {
    await save(`${output}/stability.json`, `${JSON.stringify({
      requested: Number(count),
      completed: trials.length,
      ok,
      trials,
    }, null, 2)}\n`)
  }
}

if (import.meta.main) {
  // Workflow dispatch defaults to zero; normal CI has already run its full shard.
  const [count, output] = process.argv.slice(2)
  if (count !== '0') {
    measureComposeStability(count, output).catch((error) => {
      console.error(`::error::${error.message}`)
      process.exitCode = 1
    })
  }
}
