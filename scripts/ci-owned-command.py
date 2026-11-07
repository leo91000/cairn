"""Linux-only private child subreaper for the bounded CI observer."""
import ctypes
import json
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import time


def identity(pid):
    try:
        value = Path(f'/proc/{pid}/stat').read_text()
    except OSError:
        return None
    return value[value.rfind(')') + 2:].split()[19]


def reap():
    while True:
        try:
            pid, _ = os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            return
        if pid == 0:
            return


def parent_disconnected():
    # Every direct child is either our command or an orphan adopted because of
    # subreaper ownership. Never signal by a global name or an unrelated group.
    os.killpg(os.getpgrp(), signal.SIGTERM)
    deadline = time.monotonic() + .5
    while time.monotonic() < deadline:
        try:
            children = Path(f'/proc/{os.getpid()}/task/{os.getpid()}/children').read_text().split()
        except OSError:
            children = []
        for text in children[:4096]:
            pid = int(text)
            started = identity(pid)
            if started is not None and identity(pid) == started:
                try:
                    os.kill(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
        reap()
        if not children:
            return
        time.sleep(.025)


def main():
    if sys.platform != 'linux' or os.getsid(0) != os.getpid():
        raise RuntimeError('CI ownership requires a new private Linux session')
    libc = ctypes.CDLL(None, use_errno=True)
    libc.prctl.argtypes = [ctypes.c_int] + [ctypes.c_ulong] * 4
    libc.prctl.restype = ctypes.c_int
    enabled = ctypes.c_int()
    pointer = ctypes.cast(ctypes.byref(enabled), ctypes.c_void_p).value
    if libc.prctl(36, 1, 0, 0, 0) != 0 or libc.prctl(37, pointer, 0, 0, 0) != 0 or enabled.value != 1:
        raise RuntimeError('Linux child-subreaper ownership could not be enabled')
    signal.signal(signal.SIGTERM, lambda *_: None)
    signal.signal(signal.SIGINT, lambda *_: None)
    reports = os.fdopen(3, 'w', buffering=1)
    commands = os.fdopen(4, 'r')

    def report(value):
        reports.write(json.dumps(value) + '\n')
        reports.flush()

    report(dict(type='subreaper-ready'))
    # close_fds prevents the workload from retaining either ownership channel.
    child = subprocess.Popen(sys.argv[1:], close_fds=True)
    completed = False
    while True:
        code = child.poll()
        if code is not None and not completed:
            report(dict(type='command-complete', code=code if code >= 0 else 1))
            completed = True
        if completed:
            reap()
        ready, _, _ = select.select([commands], [], [], .025)
        if not ready:
            continue
        line = commands.readline(4097)
        if not line:
            parent_disconnected()
            return 1
        if len(line) > 4096:
            raise RuntimeError('Invalid CI ownership control message')
        value = json.loads(line)
        if value.get('type') == 'release':
            return value['code'] if type(value.get('code')) is int and 0 <= value['code'] <= 255 else 1


if __name__ == '__main__':
    raise SystemExit(main())
