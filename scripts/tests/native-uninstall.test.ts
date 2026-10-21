import { spawnSync } from 'node:child_process'
import { readFileSync } from 'node:fs'
import { expect, it } from 'vitest'

const web = readFileSync(new URL('../../apps/web/src/views/Nodes.vue', import.meta.url), 'utf8')
const android = readFileSync(new URL('../../apps/android/app/src/main/java/build/cairn/app/ui/NodesScreen.kt', import.meta.url), 'utf8')
const command = web.match(/const nativeUninstallCommand = `([^`]+)`/)![1].replaceAll('\\\\', '\\')
const androidLiteral = android.match(/private const val NATIVE_UNINSTALL_COMMAND =\s*("(?:\\.|[^"\\])*")/)![1]
const script = command.slice('sudo sh -c \''.length, -1)

// Run the command shown to people, with every destructive operation replaced.
// findmnt and grep failures must stop before even a simulated removal.
function uninstall(scenario: string) {
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
grep() { if [ "$scenario" = grep-failure ]; then return 2; fi; command grep "$@"; }
rm() { echo "REMOVE $*"; [ "$scenario" != rm-failure ]; }
userdel() { echo "USERDEL $*"; [ "$scenario" != userdel-failure ]; }
id() { [ "$scenario" = userdel-failure ]; }
${script}
`], { env: { PATH: '/usr/bin:/bin', scenario }, encoding: 'utf8' })
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
  expect(uninstall('reload-failure').status).not.toBe(0)
})
