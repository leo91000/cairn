#!/usr/bin/python3
"""Synthetic Codex app-server for the disposable native workload qualification."""
import json
from collections import deque
from pathlib import Path
import subprocess
import sys
import threading


def main():
    workload = Path(__file__).with_name(sys.argv[sys.argv.index('--fixture-workload') + 1])
    lock = threading.Lock()
    process = None
    error_tail = deque(maxlen=16)
    thread_id = 'native-fixture-thread'
    turn_id = 'native-fixture-turn'

    def emit(value):
        with lock:
            print(json.dumps(value), flush=True)

    def execute():
        status = process.wait()
        turn = dict(id=turn_id, status='completed' if status == 0 else 'failed', items=[])
        if status != 0:
            # Adapter stderr is diagnostic logging, not a command-output RPC.
            # Surface a bounded synthetic workload tail in the failed turn too,
            # before controller teardown can bury it under daemon shutdown logs.
            reader.join(timeout=0.2)
            tail = b''.join(error_tail)[-16384:].decode(errors='replace')
            turn['error'] = dict(message='Synthetic native workload failed: ' + str(status) + '\n' + tail)
        emit(dict(method='turn/completed', params=dict(threadId=thread_id, turn=turn)))

    def drain():
        while chunk := process.stdout.read1(4096):
            error_tail.append(chunk)
            sys.stderr.buffer.write(chunk)
            sys.stderr.buffer.flush()

    try:
        for line in sys.stdin:
            request = json.loads(line)
            if 'id' not in request:
                continue
            method = request['method']
            result = {}
            if method == 'thread/start':
                result = dict(thread=dict(id=thread_id, turns=[]))
            elif method == 'turn/start':
                if process is not None:
                    raise RuntimeError('One workload per fixture app-server.')
                result = dict(turn=dict(id=turn_id, status='inProgress', items=[]))
            elif method == 'turn/steer':
                result = dict(turnId=turn_id)
            elif method == 'thread/unsubscribe':
                result = dict(status='unsubscribed')
            emit(dict(id=request['id'], result=result))
            if method == 'turn/start':
                # Keep app-server stdout reserved for RPC frames. The shell and
                # all its descendants remain in the real workload's cgroup.
                process = subprocess.Popen([str(workload)], stdin=subprocess.DEVNULL,
                                           stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
                reader = threading.Thread(target=drain, daemon=True)
                reader.start()
                threading.Thread(target=execute, daemon=True).start()
    finally:
        if process is not None and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


if __name__ == '__main__':
    main()
