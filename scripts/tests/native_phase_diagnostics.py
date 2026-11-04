"""Bounded observation of the explicitly disposable native CI fixture only."""
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import threading
import time
import traceback


def read(path, limit=4096):
    try:
        with Path(path).open('rb') as stream:
            return stream.read(limit).decode(errors='replace')
    except OSError:
        return None


def process_identity(pid):
    value = read(f'/proc/{pid}/stat')
    if value is None:
        return None
    fields = value[value.rfind(')') + 2:].split()
    return dict(pid=pid, state=fields[0], startTicks=fields[19],
                cpuTicks=int(fields[11]) + int(fields[12]))


class NativePhaseDiagnostics:
    def __init__(self, output, role, interval=30):
        if role not in ('reference', 'candidate'):
            raise ValueError('Unknown native diagnostic role')
        self.output, self.role, self.interval = Path(output), role, interval
        self.output.mkdir(parents=True, exist_ok=True, mode=0o700)
        self.phase = 'start'
        self.units = []
        self.entered = []
        self.lock = threading.Lock()
        self.stopped = threading.Event()
        self.started = time.monotonic()
        self.thread = threading.Thread(target=self.observe, daemon=True)
        self.thread.start()

    def enter(self, phase):
        if re.fullmatch(r'[a-z][a-z0-9-]{0,63}', phase) is None:
            raise ValueError('Unknown native diagnostic phase')
        with self.lock:
            self.phase = phase
            self.entered.append(dict(phase=phase, elapsedSeconds=time.monotonic() - self.started))
            self.entered = self.entered[-32:]

    def watch(self, supervisor, attempt):
        uuid = r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}'
        if re.fullmatch(r'cairn-native-smoke-' + uuid + r'\.service', supervisor) is None or re.fullmatch(uuid, attempt) is None:
            raise ValueError('Native diagnostic unit is outside this fixture')
        with self.lock:
            self.units = [supervisor, 'cairn-native-' + attempt + '.service']

    def unit_sample(self, unit):
        result = subprocess.run(['systemctl', 'show', unit, '--property=MainPID', '--value'],
                                capture_output=True, text=True, timeout=2)
        if result.returncode != 0 or not result.stdout.strip().isdecimal():
            return dict(available=False)
        pid = int(result.stdout.strip())
        before = process_identity(pid)
        if pid <= 1 or before is None:
            return dict(available=False)
        membership = read(f'/proc/{pid}/cgroup') or ''
        group = next((line[3:] for line in membership.splitlines() if line.startswith('0::')), None)
        group = Path('/sys/fs/cgroup') / group.lstrip('/') if group else None
        if group is not None:
            group = group.resolve()
            if not group.is_relative_to('/sys/fs/cgroup'):
                group = None
        sample = dict(**before, waitChannel=read(f'/proc/{pid}/wchan', 128),
                      kernelStack=read(f'/proc/{pid}/stack', 2048),
                      io=read(f'/proc/{pid}/io', 1024))
        if group is not None:
            sample['cgroup'] = {name: read(group / name, 2048) for name in ('cpu.stat', 'io.stat', 'memory.events')}
        after = process_identity(pid)
        return sample if after is not None and after['startTicks'] == before['startTicks'] else dict(changedDuringObservation=True)

    def snapshot(self):
        with self.lock:
            phase, units, entered = self.phase, list(self.units), list(self.entered)
        frames = sys._current_frames()
        main = threading.main_thread().ident
        ids = ([main] if main in frames else []) + [key for key in frames if key != main]
        stacks = []
        for key in ids[:16]:
            stack = []
            for frame, line in traceback.walk_stack(frames[key]):
                stack.append(dict(file=Path(frame.f_code.co_filename).name[:96], line=line,
                                  function=frame.f_code.co_name[:64]))
                if len(stack) == 12:
                    break
            stacks.append(dict(mainThread=key == main, frames=stack))
        unit_samples = []
        for index, unit in enumerate(units):
            try:
                sample = self.unit_sample(unit)
            except (OSError, ValueError, subprocess.TimeoutExpired):
                sample = dict(available=False)
            unit_samples.append(dict(role='controller' if index == 0 else 'workload', sample=sample))
        return dict(role=self.role, phase=phase, entered=entered,
                    elapsedSeconds=time.monotonic() - self.started,
                    pythonStacks=stacks, fixtureUnits=unit_samples)

    def observe(self):
        while not self.stopped.is_set():
            try:
                sample = self.snapshot()
                encoded = json.dumps(sample)
                while len(encoded.encode()) > 65536 and len(sample['pythonStacks']) > 1:
                    sample['pythonStacks'].pop()
                    sample['truncatedThreads'] = True
                    encoded = json.dumps(sample)
                temporary = self.output / 'native-phase.tmp'
                with temporary.open('w') as stream:
                    os.chmod(temporary, 0o600)
                    stream.write(encoded + '\n')
                if os.geteuid() == 0:
                    owner = self.output.stat()
                    os.chown(temporary, owner.st_uid, owner.st_gid)
                temporary.replace(self.output / 'native-phase.json')
            except (OSError, ValueError):
                # The supervisor still records process/kernel counters. A
                # failed observer is never evidence of a completed native run.
                pass
            self.stopped.wait(self.interval)

    def close(self):
        self.stopped.set()
        self.thread.join(timeout=5)


def start(role):
    output = os.environ.get('CAIRN_CI_DIAGNOSTIC_OUTPUT')
    if not output:
        raise ValueError('Native phase overlay requires its CI diagnostic supervisor')
    return NativePhaseDiagnostics(output, role)
