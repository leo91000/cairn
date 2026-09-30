// Preserve the service document, including any secret expressions, while adding
// the persistent storage required by the runner's stop markers.
import { parseDocument } from 'yaml'

export function persistentRunnerCompose(compose) {
  if (typeof compose !== 'string')
    throw new Error('Cannot verify runner storage: missing service Compose.')
  const lines = compose.split('\n')
  const start = lines.findIndex(line => /^\s+runner:\s*$/.test(line))
  if (start < 0)
    throw new Error('Cannot verify runner storage: runner service not found.')
  const indent = lines[start].search(/\S/)
  let volumeIndent
  let volumeStart
  for (let index = start + 1; index < lines.length; index++) {
    const line = lines[index]
    if (!line.trim() || line.trimStart().startsWith('#'))
      continue
    if (line.search(/\S/) <= indent)
      break
    if (volumeIndent === undefined) {
      if (/^\s+volumes:\s*$/.test(line)) {
        volumeIndent = line.search(/\S/)
        volumeStart = index
      }

      continue
    }

    if (line.search(/\S/) <= volumeIndent)
      break
    const mount = line.match(/^(\s*-\s*['"]?[^'"\s]+:\/runner-state)(?::(ro|rw))?(['"]?\s*(?:#.*)?)$/)
    if (!mount) {
      if (line.includes('/runner-state'))
        throw new Error('Cannot verify runner storage: unsupported runner-state mount.')
      continue
    }

    if (mount[2] === 'ro')
      lines[index] = `${mount[1]}:rw${mount[3]}`
    return lines.join('\n')
  }

  if (volumeStart === undefined)
    throw new Error('Cannot verify runner storage: runner volumes block not found.')
  const declarations = lines.findIndex(line => /^volumes:\s*$/.test(line))
  if (declarations < 0)
    throw new Error('Cannot verify runner storage: top-level volumes block not found.')
  let declared = false
  for (let index = declarations + 1; index < lines.length; index++) {
    if (/^\S/.test(lines[index]))
      break
    if (lines[index].startsWith('  runner-state:'))
      declared = true
  }

  if (!declared)
    lines.splice(declarations + 1, 0, '  runner-state:')
  // Locate the service block again in case top-level volumes precede services.
  const offset = !declared && declarations < volumeStart ? 1 : 0
  lines.splice(volumeStart + offset + 1, 0, `${' '.repeat(volumeIndent + 2)}- runner-state:/runner-state`)
  return lines.join('\n')
}

// Upgrade the existing service in place without parsing or serializing its
// environment expressions. Coolify retains Compose independently of the repo.
export function nativeRunnerCompose(compose) {
  const lines = persistentRunnerCompose(compose).split('\n')
  const start = lines.findIndex(line => /^\s+runner:\s*$/.test(line))
  const indent = lines[start].search(/\S/)
  for (let index = start + 1; index < lines.length; index++) {
    if (!lines[index].trim() || lines[index].trimStart().startsWith('#'))
      continue
    if (lines[index].search(/\S/) <= indent)
      break
    if (!/^\s+entrypoint:/.test(lines[index]))
      continue
    const entryIndent = lines[index].search(/\S/)
    let end = index + 1
    while (end < lines.length && lines[end].trim() && lines[end].search(/\S/) > entryIndent)
      end++
    // Coolify serializes flow sequences as block sequences. Recognize the
    // already-correct argv without rewriting its formatting on every deploy.
    const scalar = value => value.trim().replace(/\s+#.*$/, '').replace(/^(['"])(.*)\1$/, '$2')
    const value = lines[index].slice(lines[index].indexOf(':') + 1).trim()
    const args = value.startsWith('[') && value.endsWith(']')
      ? value.slice(1, -1).split(',').map(scalar)
      : value
        ? [scalar(value)]
        : lines.slice(index + 1, end).filter(line => !line.trimStart().startsWith('#')).map(line => scalar(line.replace(/^\s*-\s*/, '')))
    if ((args.length === 2 && args[0] === '/usr/local/bin/leo' && args[1] === 'runner-broker')
      || (args.length === 1 && args[0] === '/usr/local/bin/leo runner-broker')) {
      return lines.join('\n')
    }

    lines.splice(index, end - index, `${' '.repeat(entryIndent)}entrypoint: [/usr/local/bin/leo, runner-broker]`)
    return lines.join('\n')
  }

  lines.splice(start + 1, 0, `${' '.repeat(indent + 2)}entrypoint: [/usr/local/bin/leo, runner-broker]`)
  return lines.join('\n')
}

// Preserve Coolify's generated names, labels, networks and environment expressions.
// Upgrade the execution infrastructure and the manager's approved node image.
export function firecrackerRunnerCompose(compose) {
  const normalized = nativeRunnerCompose(compose)
  const document = parseDocument(normalized)
  let changed = normalized !== compose
  if (document.errors.length)
    throw new Error('Cannot migrate invalid service Compose.')
  const managerEnvironment = document.getIn(['services', 'manager', 'environment'])
  if (!managerEnvironment || typeof managerEnvironment.toJSON !== 'function' || typeof managerEnvironment.set !== 'function')
    throw new Error('Manager environment not found.')
  // Older Coolify services predate remote nodes. Their manager must advertise the
  // same immutable image digest that the release workflow assigns to LEO_IMAGE.
  // eslint-disable-next-line no-template-curly-in-string -- Compose must resolve the release image at deployment time.
  const nodeImage = '${LEO_IMAGE:-}'
  const environment = managerEnvironment.toJSON()
  if (Array.isArray(environment)) {
    const entry = `LEO_NODE_IMAGE=${nodeImage}`
    const index = environment.findIndex(value => typeof value === 'string' && value.startsWith('LEO_NODE_IMAGE='))
    if (index < 0) {
      managerEnvironment.add(entry)
      changed = true
    }
    else if (environment[index] !== entry) {
      managerEnvironment.set(index, entry)
      changed = true
    }
  }
  else if (environment && typeof environment === 'object') {
    if (managerEnvironment.get('LEO_NODE_IMAGE') !== nodeImage) {
      managerEnvironment.set('LEO_NODE_IMAGE', nodeImage)
      changed = true
    }
  }
  else {
    throw new Error('Manager environment must be a mapping or list.')
  }

  const runner = document.getIn(['services', 'runner'])
  if (!runner || typeof runner.set !== 'function')
    throw new Error('Runner service not found.')
  const mounts = runner.get('volumes')?.toJSON()
  if (!Array.isArray(mounts))
    throw new Error('Runner storage not found.')
  const data = mounts.find(mount => typeof mount === 'string' && /:\/data(?::ro|:rw)?$/.test(mount))
  const state = mounts.find(mount => typeof mount === 'string' && /:\/runner-state(?::ro|:rw)?$/.test(mount))
  if (!data || !state)
    throw new Error('Runner data and persistent disk storage are required.')
  const configuredConcurrency = Array.isArray(environment)
    ? environment.find(value => typeof value === 'string' && value.startsWith('CONCURRENCY='))?.slice('CONCURRENCY='.length)
    : environment.CONCURRENCY
  // eslint-disable-next-line no-template-curly-in-string -- Preserve the Compose environment expression.
  const concurrency = configuredConcurrency ?? '${CONCURRENCY:-4}'
  const alreadyUsesVms = runner.get('devices')?.toJSON()?.some(device => typeof device === 'string' && device.split(':')[0] === '/dev/kvm')
  const values = {
    user: '0:0',
    entrypoint: ['/usr/local/bin/leo', 'runner-broker'],
    stop_grace_period: '30s',
    read_only: true,
    cap_drop: ['ALL'],
    cap_add: ['SYS_ADMIN', 'NET_ADMIN', 'SYS_CHROOT', 'SETUID', 'SETGID', 'MKNOD', 'CHOWN', 'FOWNER', 'KILL', 'DAC_OVERRIDE'],
    security_opt: ['apparmor:unconfined', 'seccomp:unconfined'],
    devices: ['/dev/kvm:/dev/kvm', '/dev/fuse:/dev/fuse', '/dev/net/tun:/dev/net/tun'],
    sysctls: { 'net.ipv4.ip_forward': '1', 'net.ipv6.conf.all.disable_ipv6': '1' },
    tmpfs: ['/run', '/tmp'],
    environment: { DATA_DIR: '/data', CONCURRENCY: concurrency },
    volumes: [data.replace(/:(ro|rw)$/, ''), state.replace(/:(ro|rw)$/, '')],
    // Set defaults when migrating to VMs; preserve subsequent operator sizing.
    mem_limit: alreadyUsesVms ? runner.get('mem_limit') ?? '20g' : '20g',
    cpus: alreadyUsesVms ? runner.get('cpus') ?? 8 : 8,
    healthcheck: {
      test: ['CMD', 'node', '-e', 'fetch(\'http://127.0.0.1:4311/health\').then(r=>process.exit(r.ok?0:1)).catch(()=>process.exit(1))'],
      start_period: '120s',
    },
  }
  for (const [key, value] of Object.entries(values)) {
    if (JSON.stringify(runner.get(key)?.toJSON?.() ?? runner.get(key)) !== JSON.stringify(value)) {
      runner.set(key, value)
      changed = true
    }
  }

  // The old generated cap counted host vCPU threads, not guest processes.
  // Preserve explicit operator limits, but remove the former default.
  if (runner.has('pids_limit') && (!alreadyUsesVms || runner.get('pids_limit') === 256)) {
    runner.delete('pids_limit')
    changed = true
  }

  if (runner.has('privileged')) {
    runner.delete('privileged')
    changed = true
  }

  return changed ? document.toString() : compose
}
