const semver = /^\d+\.\d+\.\d+$/
const java = /^temurin-(\d+)\.(\d+)\.(\d+)\+(\d+)\.(\d+)\.LTS$/

export function validVersion(tool, version) {
  return typeof version === 'string' && (tool === 'java' ? java : semver).test(version)
}

export function newerTool(tool, current, available) {
  if (!validVersion(tool, current) || !validVersion(tool, available))
    throw new Error('Tool versions must be stable releases of the selected distribution.')
  const parts = version => tool === 'java' ? java.exec(version).slice(1).map(Number) : version.split('.').map(Number)
  const left = parts(current)
  const right = parts(available)
  for (let index = 0; index < left.length; index++) {
    if (left[index] !== right[index])
      return right[index] > left[index] ? available : current
  }

  return current
}
