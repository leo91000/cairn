"""CI-only metadata observer for the normal native controller lifecycle.

RefUnit retains systemd's exit metadata; it does not change the command, kill
policy, cgroup or timeouts. The connection must remain alive through capture.
No library is loaded or IPC performed when this module is imported.
"""
import base64
import ctypes
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import tempfile


class Inconclusive(RuntimeError):
    pass


def write_matrix(evidence, value):
    """Publish this one allowlisted artifact privately to the ordinary runner."""
    owners = []
    for key in ('CAIRN_NATIVE_EVIDENCE_UID', 'CAIRN_NATIVE_EVIDENCE_GID'):
        text = os.environ.get(key, '')
        if not re.fullmatch('[1-9][0-9]{0,9}', text) or int(text) > 2**31 - 1:
            raise Inconclusive('evidence-owner-unavailable')
        owners.append(int(text))
    uid, gid = owners
    parent = evidence.lstat()
    if not stat.S_ISDIR(parent.st_mode) or stat.S_IMODE(parent.st_mode) != 0o700:
        raise Inconclusive('evidence-directory-not-private')
    if (parent.st_uid, parent.st_gid) != (uid, gid):
        raise Inconclusive('evidence-directory-owner-mismatch')
    descriptor, temporary = tempfile.mkstemp(prefix='.native-shutdown-', dir=evidence)
    try:
        with os.fdopen(descriptor, 'w') as output:
            os.fchmod(output.fileno(), 0o600)
            os.fchown(output.fileno(), uid, gid)
            json.dump(value, output, indent=2)
            output.write('\n')
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, evidence / 'native-shutdown-matrix.json')
    finally:
        Path(temporary).unlink(missing_ok=True)


FIELDS = ('Id', 'LoadState', 'InvocationID', 'ActiveState', 'SubState',
          'MainPID', 'ExecMainPID', 'ExecMainCode', 'ExecMainStatus',
          'ExecMainExitTimestampMonotonic', 'Result', 'ControlGroup')


def properties(unit):
    result = subprocess.run(['systemctl', 'show', unit, '--property=' + ','.join(FIELDS)],
                            capture_output=True, text=True, timeout=5)
    if result.returncode or len(result.stdout) > 8192:
        raise Inconclusive('unit-properties-unavailable')
    values = dict(line.split('=', 1) for line in result.stdout.splitlines() if '=' in line)
    if set(values) != set(FIELDS) or values['Id'] != unit or values['LoadState'] != 'loaded':
        raise Inconclusive('unit-not-loaded')
    if not re.fullmatch('[0-9a-f]{32}', values['InvocationID']):
        raise Inconclusive('invocation-unavailable')
    for field in ('MainPID', 'ExecMainPID', 'ExecMainCode', 'ExecMainStatus', 'ExecMainExitTimestampMonotonic'):
        if not re.fullmatch('[0-9]{1,20}', values[field]):
            raise Inconclusive('unit-numeric-schema')
        values[field] = int(values[field])
    if values['ControlGroup'] and (not values['ControlGroup'].startswith('/') or '..' in values['ControlGroup'].split('/')):
        raise Inconclusive('unit-cgroup-schema')
    return values


def final_metadata(initial, final):
    if initial['InvocationID'] != final['InvocationID'] or initial['Id'] != final['Id']:
        raise Inconclusive('unit-invocation-changed')
    if initial['MainPID'] <= 0 or final['ExecMainPID'] != initial['MainPID']:
        raise Inconclusive('unit-primary-changed')
    if final['ActiveState'] not in ('inactive', 'failed') or final['MainPID'] != 0:
        raise Inconclusive('unit-not-terminal')
    if final['ExecMainExitTimestampMonotonic'] <= 0 or final['ExecMainCode'] not in (1, 2, 3):
        raise Inconclusive('unit-primary-exit-unavailable')
    if final['Result'] not in ('success', 'exit-code', 'signal', 'core-dump', 'timeout', 'oom-kill', 'resources'):
        raise Inconclusive('unit-result-schema')
    return {field: final[field] for field in ('InvocationID', 'ExecMainPID', 'ExecMainCode',
                                             'ExecMainStatus', 'ExecMainExitTimestampMonotonic', 'Result')}


class UnitReference:
    """Persistent IPC client, never a short-lived busctl invocation."""
    def __init__(self, unit):
        if not re.fullmatch(r'cairn-native-[0-9a-f-]{36}\.service', unit):
            raise Inconclusive('unit-name-schema')
        self.unit, self.bus, self.library = unit, ctypes.c_void_p(), None
        self.referenced = False

    def _method(self, name):
        reply = ctypes.c_void_p()
        try:
            result = self.library.sd_bus_call_method(self.bus,
                b'org.freedesktop.systemd1', b'/org/freedesktop/systemd1',
                b'org.freedesktop.systemd1.Manager', name.encode(), None,
                ctypes.byref(reply), b's', ctypes.c_char_p(self.unit.encode()))
            if result < 0:
                raise Inconclusive('unit-reference-' + name)
        finally:
            if reply:
                self.library.sd_bus_message_unref(reply)

    def __enter__(self):
        if os.geteuid() != 0 or os.environ.get('CAIRN_NATIVE_DISPOSABLE_HOST') != '1':
            raise Inconclusive('observer-not-disposable')
        self.library = ctypes.CDLL('libsystemd.so.0')
        self.library.sd_bus_open_system.argtypes = [ctypes.POINTER(ctypes.c_void_p)]
        self.library.sd_bus_open_system.restype = ctypes.c_int
        self.library.sd_bus_call_method.argtypes = [ctypes.c_void_p, ctypes.c_char_p,
            ctypes.c_char_p, ctypes.c_char_p, ctypes.c_char_p, ctypes.c_void_p,
            ctypes.POINTER(ctypes.c_void_p), ctypes.c_char_p]
        self.library.sd_bus_call_method.restype = ctypes.c_int
        self.library.sd_bus_message_unref.argtypes = [ctypes.c_void_p]
        self.library.sd_bus_message_unref.restype = ctypes.c_void_p
        self.library.sd_bus_unref.argtypes = [ctypes.c_void_p]
        self.library.sd_bus_unref.restype = ctypes.c_void_p
        try:
            if self.library.sd_bus_open_system(ctypes.byref(self.bus)) < 0:
                raise Inconclusive('observer-bus-unavailable')
            self._method('RefUnit')
            self.referenced = True
            self.initial = properties(self.unit)
            if self.initial['ActiveState'] != 'active' or self.initial['MainPID'] <= 0 or not self.initial['ControlGroup']:
                raise Inconclusive('observer-primary-not-active')
            return self
        except BaseException:
            self.close()
            raise

    def close(self):
        if not self.bus:
            return
        try:
            if self.referenced:
                self._method('UnrefUnit')
        finally:
            self.library.sd_bus_unref(self.bus)
            self.bus = ctypes.c_void_p()
            self.referenced = False

    def __exit__(self, *_args):
        self.close()

    def capture(self):
        return final_metadata(self.initial, properties(self.unit))


def workload(case):
    if case not in ('healthy', 'residual', 'primary-failure', 'cancel', 'lease', 'flush-failure'):
        raise ValueError('Invalid synthetic shutdown case.')
    script = ''
    if case == 'residual':
        script += """/usr/bin/python3 - <<'RESIDUAL' &
import ctypes, os, signal, time
from pathlib import Path
ctypes.CDLL(None).prctl(15, b'cairn-termchild', 0, 0, 0)
signal.signal(signal.SIGTERM, signal.SIG_IGN)
with open('shutdown-descendant-ready', 'w') as marker:
    marker.write('ready'); marker.flush(); os.fsync(marker.fileno())
while True: time.sleep(1)
RESIDUAL
while ! test -e shutdown-descendant-ready; do sleep 0.05; done
"""
    # Gates only the synthetic workload. All normal controller admission, engine,
    # unit launch, RootlessKit, teardown and physical storage paths are unchanged.
    script += "printf ready > shutdown-ready\nwhile ! test -e shutdown-release; do sleep 0.05; done\n"
    script += 'exit ' + ('23' if case == 'primary-failure' else '0') + '\n'
    return script


def completed(path):
    """Read bounded synthetic output in memory; retain only turn-completed bool."""
    if path.stat().st_size > 64 * 1024 * 1024:
        raise Inconclusive('fixture-log-over-budget')
    pending = b''
    found = False
    with path.open('rb') as source:
        for line in source:
            if len(line) > 65536:
                raise Inconclusive('fixture-record-over-budget')
            record = json.loads(line)
            if record.get('stderr') is not False or not isinstance(record.get('data'), str):
                continue
            pending += base64.b64decode(record['data'], validate=True)
            if len(pending) > 1024 * 1024:
                raise Inconclusive('fixture-frame-over-budget')
            parts = pending.split(b'\n')
            pending = parts.pop()
            for part in parts:
                try:
                    event = json.loads(part)
                except (ValueError, UnicodeError):
                    continue
                if isinstance(event, dict) and event.get('type') == 'turn.completed':
                    found = True
    return found


def fence(initial):
    group = Path('/sys/fs/cgroup') / initial['ControlGroup'].lstrip('/')
    events = group / 'cgroup.events'
    if not events.exists():
        return 'cgroup-absent'
    if 'populated 0' not in events.read_text().splitlines():
        raise Inconclusive('cgroup-still-populated')
    return 'cgroup-unpopulated'


def residual_identity(initial):
    group = Path('/sys/fs/cgroup') / initial['ControlGroup'].lstrip('/')
    found = {}
    for member_file in group.glob('**/cgroup.procs'):
        for value in member_file.read_text().splitlines():
            process = Path('/proc') / value
            try:
                if (process / 'comm').read_text().strip() != 'cairn-termchild':
                    continue
                fields = (process / 'stat').read_text().rsplit(')', 1)[1].split()
                found[int(value)] = dict(pid=int(value), startTicks=int(fields[19]))
            except FileNotFoundError:
                continue
    if len(found) != 1:
        raise Inconclusive('owned-descendant-not-identified')
    return next(iter(found.values()))


def child_gone(child):
    if child is None:
        return None
    try:
        fields = (Path('/proc') / str(child['pid']) / 'stat').read_text().rsplit(')', 1)[1].split()
        return int(fields[19]) != child['startTicks']
    except FileNotFoundError:
        return True


def control_observation(result):
    """Capture completion is separate from whether a control matched its goal."""
    case = result['case']
    healthy = result['StatusCode'] == 0 and result['providerCompleted'] and result['PhysicalFinalized']
    if case.startswith('healthy-'):
        return dict(status='matched' if healthy else 'not-matched', target='normal-controller-completion')
    if case == 'residual':
        primary = result['primary']
        candidate = result['providerCompleted'] and (primary['ExecMainCode'], primary['ExecMainStatus'], primary['Result']) == (1, 0, 'timeout')
        return dict(status='not-attributed' if candidate else 'not-exercised',
            target='owned-descendant-alive-after-systemd-primary-exit',
            primarySuccessWithCleanupTimeout=candidate, descendantAliveAfterPrimaryExitProven=False,
            explanation='Pre-release membership and post-fence absence do not establish residual lifetime. Chat process-group or PID-namespace cleanup may remove it earlier.')
    if case == 'primary-failure':
        matched = not result['providerCompleted'] and result['StatusCode'] != 0 and result['PhysicalFinalized']
        return dict(status='matched' if matched else 'not-matched', target='genuine-logical-workload-failure')
    if case in ('cancel', 'lease'):
        matched = not result['providerCompleted'] and result['StatusCode'] == 143 and result['PhysicalFinalized']
        return dict(status='matched' if matched else 'not-matched', target=case + '-controller-stop')
    if case == 'flush-failure':
        matched = result['StatusCode'] == 70 and result['FinalizationFailed']
        return dict(status='matched' if matched else 'not-matched', target='typed-physical-flush-failure')
    raise Inconclusive('unexpected-control-case')


def run_matrix(*, create, start, managed, api, fenced, manager, state, eventually, command, evidence, start_controller):
    """Only invoked by the real explicitly disposable native-runtime fixture."""
    results = []
    def persist(status, stage):
        value = dict(schemaVersion=1, status=status, stage=stage, cases=results, causalRedEstablished=False,
            interpretation='Observation only; primary and cleanup outcomes remain distinct. No mission driver or live recovery claim.')
        write_matrix(evidence, value)
        return value

    for name in ('healthy-unpinned', 'healthy-pinned', 'residual', 'primary-failure', 'cancel', 'lease', 'flush-failure'):
        persist('running', name)
        case = 'healthy' if name.startswith('healthy-') else name
        run = create()
        if not results:
            start_controller()
        attempt = start(run, long=False, shutdown_case=case, lease=90000)
        eventually(lambda: (managed(run) / 'shutdown-ready').is_file())
        unit = 'cairn-native-' + attempt + '.service'
        initial = properties(unit)
        reference = None
        try:
            if name != 'healthy-unpinned':
                reference = UnitReference(unit).__enter__()
                initial = reference.initial
            if initial['ActiveState'] != 'active' or initial['MainPID'] <= 0 or not initial['ControlGroup']:
                raise Inconclusive('matrix-primary-not-active')
            child = residual_identity(initial) if case == 'residual' else None
            workspace = managed(run)
            if case == 'flush-failure':
                mount = state / 'native' / run / 'data'
                attachment = state / 'native-block' / run / 'root/disk.blk'
                device = mount.stat().st_dev
                if not stat.S_ISBLK(attachment.stat().st_mode) or attachment.stat().st_rdev != device:
                    raise Inconclusive('fault-device-unattributed')
                sys_device = (Path('/sys/dev/block') / f'{os.major(device)}:{os.minor(device)}').resolve(strict=True)
                if not sys_device.name.startswith('ublkb'):
                    raise Inconclusive('fault-not-owned-ublk')
                command('mount', '-o', 'remount,errors=remount-ro', str(mount))
                command('sync', '-f', str(mount))
                (Path('/sys/fs/ext4') / sys_device.name / 'trigger_fs_error').write_text('cairn-disposable-flush-failure\n')
                api('/runs/' + attempt, method='DELETE')
            elif case == 'cancel':
                api('/runs/' + attempt, method='DELETE')
            elif case == 'lease':
                api('/runs/' + attempt + '/lease', dict(remainingMs=1000))
            else:
                (workspace / 'shutdown-release').touch()
            fenced(run, attempt, expect_flush_failure=case == 'flush-failure')
            record = json.loads((state / (attempt + '.exit')).read_text())
            if not isinstance(record, dict) or type(record.get('StatusCode')) is not int:
                raise Inconclusive('controller-evidence-schema')
            result = dict(case=name, attempt=attempt, providerCompleted=completed(state / (attempt + '.log')),
                StatusCode=record['StatusCode'], PhysicalFinalized=record.get('PhysicalFinalized') is True,
                FinalizationFailed=record.get('FinalizationFailed') is True,
                primary=reference.capture() if reference else None, fence=fence(initial), ownedDescendant=child)
            result['ownedDescendantGone'] = child_gone(child)
            if result['ownedDescendantGone'] is False:
                raise Inconclusive('owned-descendant-remains')
            point = manager('publish', run=run)
            storage = api('/disks/' + run + '/storage-status')
            result['backup'] = dict(published=bool(point.get('backupId')), dirtyBytes=storage['dirtyBytes'])
            result['controlObservation'] = control_observation(result)
            results.append(result)
            persist('running', name + '-captured')
        except BaseException as error:
            persist('inconclusive', error.args[0] if isinstance(error, Inconclusive) else 'fixture-control-failed')
            raise
        finally:
            if reference is not None:
                try:
                    reference.close()
                except Inconclusive:
                    persist('inconclusive', 'observer-close-failed')
                    raise
    healthy = results[:2]
    for result in healthy:
        if result['StatusCode'] != 0 or not result['providerCompleted'] or not result['PhysicalFinalized']:
            raise Inconclusive('healthy-control-failed')
    pinned = healthy[1]['primary']
    if (pinned['ExecMainCode'], pinned['ExecMainStatus'], pinned['Result']) != (1, 0, 'success'):
        raise Inconclusive('healthy-primary-not-successful')
    return persist('observed', 'all-cases-captured')
