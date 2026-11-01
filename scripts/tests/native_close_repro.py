"""Opt-in real native close stress; a timeout is evidence, not a cause verdict."""
import fcntl
import json
import threading
import time
import urllib.error


def workload(attempt):
    # This delays provider completion, never a controller close phase. Once the
    # fixture releases it the synthetic app-server emits normal turn/completed.
    return """/usr/bin/python3 - <<'CLOSE_WORKLOAD'
import os,time
from pathlib import Path
gate=Path('close-complete-%s')
with open('close-payload','wb') as output:
    for sequence in range(512):
        output.seek((sequence%%128)*4096)
        output.write(bytes([sequence%%251])*4096)
        output.flush(); os.fsync(output.fileno())
        if gate.exists(): break
        time.sleep(0.01)
    while not gate.exists(): time.sleep(0.01)
CLOSE_WORKLOAD
""" % attempt


def physical_proof(state, run, attempt):
    """Read/acquire only fixture-owned state after the real /wait completes."""
    logical = state / 'disks' / run
    physical = logical
    pointer = logical / 'environment'
    if pointer.exists():
        physical = state / 'environments' / pointer.read_text().strip()
    locks = [state / 'native' / run / 'owner.lock',
             logical / 'ownership.lock', physical / 'lock']
    for path in locks:
        # Do not create a missing lock: missing state would invalidate the proof.
        with path.open('rb') as handle:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
            fcntl.flock(handle, fcntl.LOCK_UN)
    receipts = []
    for path in (state / 'ublk-devices').glob('*.json'):
        record = json.loads(path.read_text())
        if record.get('vmId') == run:
            receipts.append(path.name)
    attachment = state / 'native-block' / run / 'root/disk.blk'
    assert not receipts, 'Exact run retained a ublk receipt after terminal release.'
    assert not attachment.exists(), 'Exact run retained its private block attachment.'
    assert not (state / (attempt + '.active')).exists()
    return dict(executionLeaseFree=True, logicalLeaseFree=True, physicalLeaseFree=True,
                ownUblkReceiptAbsent=True, privateAttachmentAbsent=True, activeAbsent=True)


def qualify(state, run, start, api, fenced, managed, metrics, evidence):
    """Same host, workload and real close; only capture scheduling changes.

    Request submission timestamps prove the schedule, not server-side overlap.
    Source-owned phase observations are needed to classify a stalled await.
    """
    samples = metrics.setdefault('nativeCloseRepro', [])
    evidence.mkdir(parents=True, exist_ok=True)
    report = evidence / 'native-close-repro.json'

    def save():
        report.write_text(json.dumps(dict(schemaVersion=1, conditions=metrics['conditions'],
                                         samples=samples), indent=2) + '\n')
        report.chmod(0o600)

    for repetition in range(2):
        for mode in ('no-capture', 'completed-captures', 'completion-race'):
            sample = dict(mode=mode, repetition=repetition, captures=[],
                          status='starting', overlapProven=False)
            samples.append(sample)
            save()
            attempt = start(run, long=False, close_repro=True)
            sample['attempt'] = attempt
            sample['status'] = 'workload-ready'
            save()

            def capture():
                observed = dict(submitted=time.monotonic())
                sample['captures'].append(observed)
                try:
                    value = api('/runs/' + attempt + '/snapshot', dict(consistency='crash'), timeout=60)
                    observed.update(finished=time.monotonic(), status='completed',
                                    generation=value['manifest']['generation'])
                except urllib.error.HTTPError as error:
                    observed.update(finished=time.monotonic(), status='http-error', code=error.code)
                except Exception as error:
                    # Never store exception messages: an RPC error can carry a grant.
                    observed.update(finished=time.monotonic(), status='error', kind=type(error).__name__)

            if mode != 'no-capture':
                for _ in range(2):
                    capture()
                save()
                assert all(item['status'] == 'completed' for item in sample['captures']), \
                    'Pre-completion crash-consistent capture did not complete.'
            pending = None
            if mode == 'completion-race':
                pending = threading.Thread(target=capture, daemon=True)
                pending.start()
            sample['completionReleased'] = time.monotonic()
            (managed(run) / ('close-complete-' + attempt)).touch()
            sample['status'] = 'provider-completion-released'
            save()
            try:
                completed = fenced(run, attempt, timeout=90)
                result = api('/runs/' + attempt + '/wait')
                assert result['StatusCode'] == 0, 'Normal synthetic provider completion failed.'
                sample['terminalObserved'] = time.monotonic()
                sample['physical'] = physical_proof(state, run, attempt)
                sample['completedGeneration'] = completed['manifest']['generation']
                if pending:
                    pending.join(timeout=65)
                    assert not pending.is_alive(), 'Concurrent capture RPC remained pending.'
                if mode == 'completed-captures':
                    assert sample['completedGeneration'] > max(item['generation'] for item in sample['captures'])
                # A race capture may reach the controller after stop and return
                # 409. Preserve that observation; do not call it proven overlap.
                if mode == 'completion-race':
                    assert sample['captures'][-1]['status'] == 'completed' or (
                        sample['captures'][-1]['status'] == 'http-error'
                        and sample['captures'][-1].get('code') == 409
                    ), 'Concurrent capture failed outside the normal stop race.'
                sample['status'] = 'passed'
            except Exception as error:
                sample.update(status='failed', failureKind=type(error).__name__,
                              activePresent=(state / (attempt + '.active')).exists(),
                              exitPresent=(state / (attempt + '.exit')).exists())
                raise
            finally:
                save()
