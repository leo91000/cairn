import process from 'node:process'

export function validationFailures(needs) {
  const failures = []
  for (const name of ['scope', 'quality']) {
    if (needs[name]?.result !== 'success')
      failures.push(`${name} did not pass`)
  }

  for (const name of ['contracts', 'android', 'browser']) {
    const expected = needs.scope?.outputs?.[name] === 'true' ? 'success' : 'skipped'
    if (needs[name]?.result !== expected)
      failures.push(`${name}: expected ${expected}, got ${needs[name]?.result}`)
  }

  return failures
}

if (import.meta.main) {
  try {
    const failures = validationFailures(JSON.parse(process.env.RESULTS ?? '{}'))
    if (failures.length)
      throw new Error(failures.join('\n'))
  }
  catch (error) {
    console.error(error instanceof Error ? error.message : 'PR validation results are invalid.')
    process.exitCode = 1
  }
}
