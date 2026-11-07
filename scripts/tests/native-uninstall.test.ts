import { spawnSync } from 'node:child_process'
import {
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { expect, it } from 'vitest'

const web = readFileSync(new URL('../../apps/web/src/views/Nodes.vue', import.meta.url), 'utf8')
const android = readFileSync(new URL('../../apps/android/app/src/main/java/build/cairn/app/ui/NodesScreen.kt', import.meta.url), 'utf8')
const command = web.match(/const nativeUninstallCommand = `([^`]+)`/)![1].replaceAll('\\\\', '\\')
const androidLiteral = android.match(/private const val NATIVE_UNINSTALL_COMMAND =\s*("(?:\\.|[^"\\])*")/)![1]
const script = command.slice('sudo sh -c \''.length, -1)

// Run the command shown to people, with every destructive operation replaced.
// findmnt and grep failures must stop before even a simulated removal.
const policyHeader = readFileSync(new URL('../../crates/installation/src/native/mode.rs', import.meta.url), 'utf8').match(/const SUDO_HEADER: &str = "([^"]+)";/)![1]

function uninstall(scenario: string, policies: Record<string, string> = {}) {
  const directory = mkdtempSync(join(tmpdir(), 'cairn-uninstall-policies-'))
  for (const [name, contents] of Object.entries(policies))
    writeFileSync(join(directory, name), contents)
  const nodeUnit = join(directory, 'firecracker-node-root')
  if (scenario === 'vm-installed')
    writeFileSync(nodeUnit, 'fixture')
  const isolatedScript = script.replaceAll('/etc/sudoers.d', '"$CAIRN_TEST_POLICY_DIR"')
    .replaceAll('/var/lib/cairn-node', '"$CAIRN_TEST_NODE_UNIT"')
  try {
    return spawnSync('sh', ['-c', `
systemctl() { echo "SYSTEMCTL $*"; [ "$scenario" != reload-failure ]; }
findmnt() {
  case "$scenario" in
    findmnt-failure) return 2 ;;
    findmnt-missing) return 127 ;;
    mounted) echo /var/lib/cairn-native/state/native/run/data ;;
    *) echo / ;;
  esac
}
grep() {
  if [ "$scenario" = grep-failure ]; then return 2; fi
  if [ "$scenario" = policy-read-failure ]; then
    case "$*" in *cairn-native-1000*) return 2 ;; esac
  fi
  command grep "$@"
}
rm() {
  echo "REMOVE $*"
  if [ "$scenario" = policy-delete-failure ]; then
    case "$*" in *cairn-native-1000*) return 1 ;; esac
  fi
  [ "$scenario" != rm-failure ]
}
userdel() { echo "USERDEL $*"; [ "$scenario" != userdel-failure ]; }
id() { [ "$scenario" = userdel-failure ]; }
${isolatedScript}
`], {
      env: {
        PATH: '/usr/bin:/bin',
        scenario,
        CAIRN_TEST_POLICY_DIR: directory,
        CAIRN_TEST_NODE_UNIT: nodeUnit,
      },
      encoding: 'utf8',
    })
  }
  finally {
    rmSync(directory, { recursive: true, force: true })
  }
}

it('shows the same shell command on web and Android', () => {
  expect(JSON.parse(androidLiteral.replaceAll('\\$', '$'))).toBe(command)
  expect(spawnSync('sh', ['-n', '-c', script]).status).toBe(0)
})

it.each(['findmnt-failure', 'findmnt-missing', 'grep-failure', 'mounted'])('keeps files when mount safety cannot be established: %s', (scenario) => {
  const result = uninstall(scenario)
  expect(result.status).not.toBe(0)
  expect(result.stdout).not.toContain('REMOVE')
  expect(result.stdout).not.toContain('USERDEL')
})

it.each(['rm-failure', 'userdel-failure'])('keeps the unit when cleanup fails: %s', (scenario) => {
  const result = uninstall(scenario)
  expect(result.status).not.toBe(0)
  expect(result.stdout).not.toContain('REMOVE -f /etc/systemd/system/cairn-native-node.service')
})

it('finishes an unmounted uninstall and reports a failed daemon reload', () => {
  expect(uninstall('success').status).toBe(0)
  expect(uninstall('success').stdout).toContain('REMOVE -f /etc/systemd/system/cairn-native-node.service')
  // Without a Firecracker node left on the machine, the shared gateway goes too.
  expect(uninstall('success').stdout).toContain('REMOVE -rf /etc/systemd/system/cairn-node-gateway.service /opt/cairn-gateway /var/lib/cairn-gateway')
  expect(uninstall('reload-failure').status).not.toBe(0)
})

it('keeps the shared gateway while a VM runtime is still installed', () => {
  const result = uninstall('vm-installed')
  expect(result.status).toBe(0)
  expect(result.stdout).not.toContain('REMOVE -rf /etc/systemd/system/cairn-node-gateway.service')
})

it('removes owned sudo grants across UID changes before deleting their account', () => {
  expect(command).toContain(policyHeader)
  const owned = `${policyHeader}\ncairn-workload ALL=(ALL:ALL) NOPASSWD: ALL\n`
  const result = uninstall('success', {
    'cairn-native-1000': owned,
    'cairn-native-2000': owned,
    'cairn-native-3000': `${policyHeader}\nother-workload ALL=(ALL:ALL) NOPASSWD: ALL\n`,
    'cairn-native-4000': 'cairn-workload ALL=(ALL:ALL) NOPASSWD: ALL\n',
  })
  expect(result.status).toBe(0)
  const removals = result.stdout.split('\n').filter(line => line.startsWith('REMOVE -f -- '))
  expect(removals).toHaveLength(2)
  expect(removals.some(line => line.endsWith('/cairn-native-1000'))).toBe(true)
  expect(removals.some(line => line.endsWith('/cairn-native-2000'))).toBe(true)
  expect(result.stdout.indexOf(removals[1])).toBeLessThan(result.stdout.indexOf('USERDEL'))
})

it.each(['policy-read-failure', 'policy-delete-failure'])('keeps account and state when sudo cleanup fails: %s', (scenario) => {
  const result = uninstall(scenario, {
    'cairn-native-1000': `${policyHeader}\ncairn-workload ALL=(ALL:ALL) NOPASSWD: ALL\n`,
  })
  expect(result.status).not.toBe(0)
  expect(result.stdout).not.toContain('USERDEL')
  expect(result.stdout).not.toContain('REMOVE -rf /opt/cairn-native')
})
