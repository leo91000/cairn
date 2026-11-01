import process from 'node:process'

export function validationFailures(needs) {
  const failures = []
  for (const name of ['scope', 'quality']) {
    if (needs[name]?.result !== 'success')
      failures.push(`${name} did not pass`)
  }

  for (const name of ['contracts', 'android', 'browser']) {
    const selected = needs.scope?.outputs?.[name]
    let expected = 'success'
    if (selected === 'false')
      expected = 'skipped'
    else if (selected !== 'true')
      failures.push(`${name}: scope output must be exactly "true" or "false"`)

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
