"""Fixed-code observation for the disposable native CI fixture only."""
import json
import re


REASONS = {
    'Node admission is reserved for another execution.': 'exclusive_reservation',
    'Attempt has already stopped.': 'attempt_stopped',
    'Native execution lease is missing or expired.': 'lease_expired',
    'Native privilege generation is not confirmed; placement must wait.': 'mode_generation',
    "Native plan exceeds the node owner's configured resource budget.": 'resource_budget',
    'Retained native work leaves insufficient shared disk budget for this preparation.': 'retained_pool_budget',
    'Native workspace is still active.': 'workspace_owner_busy',
    'Native block restore must finish before execution resumes.': 'restore_pending',
    'Native disk is active.': 'disk_owner_busy',
    'This operation conflicts with an existing record or active run.': 'record_conflict',
}
ADMISSION = re.compile(r'/runs/([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})\Z')


def rejection_signal(path, method, status, payload, role):
    """Never return payload text, request data, headers or arbitrary paths."""
    if role not in ('reference', 'candidate') or method != 'POST' or status != 409:
        return None
    match = ADMISSION.fullmatch(path) if isinstance(path, str) else None
    if match is None or not isinstance(payload, bytes):
        return None
    reason = 'unrecognized'
    if len(payload) <= 4096:
        try:
            value = json.loads(payload)
        except (ValueError, UnicodeError, RecursionError):
            value = None
        if type(value) is dict and set(value) == {'error'} and type(value['error']) is str:
            reason = REASONS.get(value['error'], reason)
    return dict(role=role, method='POST', route='native_attempt_admission',
                attempt=match.group(1), status=409, reason=reason, responseBytes=len(payload))


def report_rejection(path, method, status, payload, role):
    signal = rejection_signal(path, method, status, payload, role)
    if signal is not None:
        try:
            print('NATIVE_CI_ADMISSION_REJECTION ' + json.dumps(signal, sort_keys=True), flush=True)
        except (OSError, ValueError):
            # Observing a rejection must not replace its original HTTP error.
            pass
