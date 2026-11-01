"""Opt-in real native close stress; a timeout is evidence, not a cause verdict."""
import errno
import fcntl
import http.client
import io
import json
import selectors
import socket
import time


class Inconclusive(Exception):
    def __init__(self, stage):
        super().__init__(stage)
        self.stage = stage


class Rejected(Exception):
    def __init__(self, code):
        super().__init__('Fixture request rejected')
        self.code = code


def exit_metadata(value):
    """Only fixed numeric/bool controller fields; never retain unknown content."""
    if type(value) is int:
        code, flags = value, {}
    elif isinstance(value, dict):
        code, flags = value.get('StatusCode'), value
    else:
        return dict(schemaValid=False)
    if type(code) is not int or not 0 <= code <= 255:
        return dict(schemaValid=False)
    return dict(schemaValid=True, StatusCode=code,
                FinalizationFailed=flags.get('FinalizationFailed') is True and code == 70,
                PhysicalFinalized=flags.get('PhysicalFinalized') is True)


class BufferedResponse:
    def __init__(self, data):
        self.data = data

    def makefile(self, *_args):
        return io.BytesIO(self.data)


class Request:
    def __init__(self, path, deadline, stage):
        self.path, self.deadline, self.stage = path, deadline, stage
        self.done, self.error, self.value = False, None, None
        self.received, self.sent = bytearray(), 0
        self.submitted = time.monotonic()


class Client:
    """Owned nonblocking Unix HTTP requests: one pump, no threads or workers.

    Connection: close gives an unambiguous response boundary even for /wait's
    heartbeat stream. Every select/send/recv shares the monotonic request bound.
    All sockets are explicitly unregistered and closed on success or exception.
    """
    def __init__(self, state, token):
        self.state, self.token = state, token
        self.selector = selectors.DefaultSelector()
        self.active = set()
        self.lease_enabled, self.renewal = False, None
        self.lease_ack, self.lease_count = None, 0
        self.startup_exit = None

    def request(self, path, body=None, *, deadline, stage):
        request = Request(path, deadline, stage)
        payload = json.dumps(body or {}).encode()
        request.outgoing = (f'POST {path} HTTP/1.1\r\nHost: fixture\r\n'
            f'Authorization: Bearer {self.token}\r\nContent-Type: application/json\r\n'
            f'Content-Length: {len(payload)}\r\nConnection: close\r\n\r\n').encode() + payload
        request.socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.active.add(request)
        try:
            request.socket.setblocking(False)
            result = request.socket.connect_ex(str(self.state / 'controller.sock'))
            if result not in (0, errno.EINPROGRESS, errno.EAGAIN):
                raise Inconclusive(stage)
            self.selector.register(request.socket, selectors.EVENT_READ | selectors.EVENT_WRITE, request)
        except Exception:
            self._finish(request, Inconclusive(stage))
        return request

    def _finish(self, request, error=None):
        try:
            self.selector.unregister(request.socket)
        except KeyError:
            pass
        request.socket.close()
        self.active.discard(request)
        request.done, request.error, request.finished = True, error, time.monotonic()

    def _advance(self, request, events):
        try:
            if events & selectors.EVENT_WRITE and request.sent < len(request.outgoing):
                if request.socket.getsockopt(socket.SOL_SOCKET, socket.SO_ERROR):
                    raise Inconclusive(request.stage)
                request.sent += request.socket.send(request.outgoing[request.sent:])
                if request.sent == len(request.outgoing):
                    self.selector.modify(request.socket, selectors.EVENT_READ, request)
            if events & selectors.EVENT_READ:
                chunk = request.socket.recv(65536)
                if chunk:
                    request.received.extend(chunk)
                    if len(request.received) > 4 * 1024 * 1024:
                        raise Inconclusive(request.stage)
                    return
                response = http.client.HTTPResponse(BufferedResponse(bytes(request.received)))
                response.begin()
                if response.status >= 400:
                    raise Rejected(response.status)
                request.value = json.loads(response.read())
                self._finish(request)
        except BlockingIOError:
            pass
        except Rejected as error:
            self._finish(request, error)
        except Exception:
            self._finish(request, Inconclusive(request.stage))

    def _lease_step(self):
        now = time.monotonic()
        if self.renewal and self.renewal.done:
            if self.renewal.error or self.renewal.value.get('ok') is not True:
                raise Inconclusive('lease_renewal')
            self.lease_ack = self.renewal.finished
            self.lease_count += 1
            self.lease_due = now + 15
            self.renewal = None
        if not self.lease_enabled:
            return
        if now >= self.hold_deadline:
            raise Inconclusive('held_provider_deadline')
        if now >= self.lease_due and self.renewal is None:
            self.renewal = self.request('/runs/' + self.attempt + '/lease',
                dict(remainingMs=90000), deadline=min(now + 10, self.hold_deadline), stage='lease_renewal')

    def pump(self, deadline):
        now = time.monotonic()
        for request in list(self.active):
            if now >= request.deadline:
                self._finish(request, Inconclusive(request.stage))
        self._lease_step()
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return
        for key, events in self.selector.select(min(remaining, 0.1)):
            self._advance(key.data, events)
        self._lease_step()

    def wait(self, request, deadline):
        deadline = min(deadline, request.deadline)
        while not request.done and time.monotonic() < deadline:
            self.pump(deadline)
        if not request.done:
            self._finish(request, Inconclusive(request.stage))
        if request.error:
            raise request.error
        return request.value

    def api(self, path, body=None, *, deadline, stage):
        return self.wait(self.request(path, body, deadline=deadline, stage=stage), deadline)

    def until(self, check, deadline, stage):
        while time.monotonic() < deadline:
            try:
                if check():
                    return
            except FileNotFoundError:
                pass
            self.pump(deadline)
        raise Inconclusive(stage)

    def arm(self, attempt, ttl, deadline):
        assert ttl == 90000
        self.attempt, self.hold_deadline = attempt, deadline
        value = self.api('/runs/' + attempt + '/lease', dict(remainingMs=ttl),
                         deadline=min(time.monotonic() + 10, deadline), stage='lease_initial')
        if value.get('ok') is not True:
            raise Inconclusive('lease_initial')
        self.lease_ack, self.lease_count = time.monotonic(), 1
        self.lease_due, self.lease_enabled = self.lease_ack + 15, True

    def release_lease(self):
        # Refresh the same normal 90s grant before releasing provider completion;
        # do not lengthen its TTL, and stop all renewal at this release boundary.
        if self.renewal:
            self.wait(self.renewal, self.hold_deadline)
        self.lease_enabled = False
        value = self.api('/runs/' + self.attempt + '/lease', dict(remainingMs=90000),
            deadline=min(time.monotonic() + 10, self.hold_deadline), stage='lease_release_refresh')
        if value.get('ok') is not True:
            raise Inconclusive('lease_release_refresh')
        self.lease_ack = time.monotonic()
        self.lease_count += 1

    def close(self):
        self.lease_enabled = False
        for request in list(self.active):
            self._finish(request, Inconclusive('request_cancelled'))
        self.selector.close()


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


def qualify(state, run, start, verify_terminal, managed, metrics, evidence, client_factory):
    """Same host, workload and real close; only capture scheduling changes.

    Request submission timestamps prove the schedule, not server-side overlap.
    Source-owned phase observations are needed to classify a stalled await.
    """
    samples = metrics.setdefault('nativeCloseRepro', [])
    evidence.mkdir(parents=True, exist_ok=True)
    report = evidence / 'native-close-repro.json'

    def save():
        status = 'passed' if len(samples) == 6 and all(row['status'] == 'passed' for row in samples) else 'inconclusive'
        if samples and samples[-1]['status'] == 'failed':
            status = 'failed'
        report.write_text(json.dumps(dict(schemaVersion=1, conditions=metrics['conditions'],
                                         status=status, samples=samples), indent=2) + '\n')
        report.chmod(0o600)

    for repetition in range(2):
        for mode in ('no-capture', 'completed-captures', 'completion-race'):
            sample = dict(mode=mode, repetition=repetition, captures=[],
                          status='starting', stage='startup', overlapProven=False)
            samples.append(sample)
            save()
            client, pending, attempt = client_factory(), None, None
            stage = 'startup'

            def settle_capture(request, deadline):
                observed = dict(submitted=request.submitted)
                try:
                    value = client.wait(request, deadline)
                    observed.update(finished=time.monotonic(), status='completed',
                                    generation=value['manifest']['generation'])
                except Rejected as error:
                    observed.update(finished=time.monotonic(), status='http-error', code=error.code)
                except Inconclusive as error:
                    observed.update(finished=time.monotonic(), status='inconclusive', stage=error.stage)
                    raise
                finally:
                    sample['captures'].append(observed)
                return observed

            def stop_reason():
                if not attempt:
                    return None
                path = state / (attempt + '.stop-reason.json')
                if not path.exists():
                    return None
                reason = json.loads(path.read_text()).get('reason')
                return reason if reason in ('lease_expired', 'time_limit', 'storage_protection_failed',
                                             'controller_shutdown', 'cancelled') else 'other'

            try:
                attempt = start(run, long=False, close_repro=True, diagnostic=client)
                sample.update(attempt=attempt, status='workload-ready')
                save()
                stage = 'precompletion_capture'
                sample['stage'] = stage
                save()
                if mode != 'no-capture':
                    for _ in range(2):
                        deadline = min(time.monotonic() + 60, client.hold_deadline)
                        request = client.request('/runs/' + attempt + '/snapshot', dict(consistency='crash'),
                                                 deadline=deadline, stage=stage)
                        observed = settle_capture(request, deadline)
                        save()
                        if observed['status'] != 'completed':
                            raise Inconclusive(stage)
                stage = 'lease_release_refresh'
                sample['stage'] = stage
                save()
                client.release_lease()
                if stop_reason():
                    raise Inconclusive('workload_stopped_before_release')
                if mode == 'completion-race':
                    pending = client.request('/runs/' + attempt + '/snapshot', dict(consistency='crash'),
                        deadline=time.monotonic() + 60, stage='race_capture')
                    # Start transmitting while the workload is held, then release
                    # immediately. The real close is never paused for the capture.
                    client.pump(time.monotonic() + 0.01)
                sample['completionReleased'] = time.monotonic()
                deadline = sample['completionReleased'] + 90
                sample['terminalDeadline'] = deadline
                (managed(run) / ('close-complete-' + attempt)).touch()
                sample.update(status='provider-completion-released',
                              lease=dict(ttlMs=90000, grants=client.lease_count,
                                         lastAcknowledged=client.lease_ack, renewalStopped=True))
                save()
                stage = 'terminal_release'
                sample['stage'] = stage
                save()
                result = client.api('/runs/' + attempt + '/wait', deadline=deadline, stage=stage)
                sample['terminalResult'] = exit_metadata(result)
                if not sample['terminalResult']['schemaValid']:
                    raise Inconclusive('terminal_exit_metadata')
                if stop_reason():
                    raise Inconclusive('workload_stopped')
                if result['StatusCode'] != 0:
                    raise Inconclusive('normal_provider_completion')
                verify_terminal(run, attempt, result, deadline=deadline)
                sample['terminalObserved'] = time.monotonic()
                # Settle the overlapping request only after the real terminal,
                # before requesting the completed prefix's same snapshot.lock.
                stage = 'race_capture'
                sample['stage'] = stage
                save()
                if pending:
                    observed = settle_capture(pending, deadline)
                    if not (observed['status'] == 'completed' or
                            (observed['status'] == 'http-error' and observed.get('code') == 409)):
                        raise Inconclusive(stage)
                stage = 'physical_release'
                sample['stage'] = stage
                save()
                sample['physical'] = physical_proof(state, run, attempt)
                if time.monotonic() >= deadline:
                    raise Inconclusive(stage)
                stage = 'completed_prefix'
                sample['stage'] = stage
                save()
                completed = client.api('/disks/' + run + '/snapshot-completed', deadline=deadline, stage=stage)
                assert completed['manifest']['kind'] == 'native-ext4-v1'
                assert completed['manifest']['onDemand'] is True
                sample['completedGeneration'] = completed['manifest']['generation']
                if mode == 'completed-captures':
                    assert sample['completedGeneration'] > max(item['generation'] for item in sample['captures'])
                if time.monotonic() >= deadline:
                    raise Inconclusive(stage)
                if stop_reason():
                    raise Inconclusive('workload_stopped')
                sample.update(status='passed', stage='completed')
            except Inconclusive as error:
                sample.update(status='inconclusive', stage=error.stage)
                raise
            except Rejected as error:
                sample.update(status='inconclusive', stage=stage, httpStatus=error.code)
                raise Inconclusive(stage) from None
            except Exception as error:
                status = 'failed' if stage in ('terminal_release', 'physical_release') else 'inconclusive'
                sample.update(status=status, stage=stage, failureKind=type(error).__name__)
                raise
            finally:
                client.close()
                attempt = attempt or getattr(client, 'attempt', None)
                if attempt:
                    sample['attempt'] = attempt
                sample['ownedRequestsRemaining'] = len(client.active)
                if client.startup_exit is not None:
                    sample['startupExit'] = client.startup_exit
                sample['stopReason'] = stop_reason()
                if attempt:
                    sample.update(activePresent=(state / (attempt + '.active')).exists(),
                                  exitPresent=(state / (attempt + '.exit')).exists())
                save()
