"""Select an immutable comparison commit for every supported CI event."""
import os
import re
import subprocess


def valid_sha(value):
    return re.fullmatch(r'[0-9a-f]{40}', value) is not None and value != '0' * 40


def git(*args, cwd=None):
    return subprocess.run(['git', *args], cwd=cwd, check=True, capture_output=True, text=True).stdout.strip()


def select_reference(preferred, current, cwd=None):
    if preferred and preferred != '0' * 40:
        if not valid_sha(preferred):
            raise ValueError('Native reference must be a nonzero full commit SHA.')
        return preferred
    if not valid_sha(current):
        raise ValueError('Native reference fallback requires checkout of the exact workflow commit.')
    # workflow_dispatch has no before/base; newly created tags have zero before.
    # Checkout is shallow, so fetch the candidate and its first parent explicitly.
    git('fetch', '--no-tags', '--depth=2', 'origin', current, cwd=cwd)
    commit = git('rev-parse', '--verify', current + '^{commit}', cwd=cwd)
    if git('rev-parse', 'HEAD', cwd=cwd) != commit:
        raise ValueError('Native reference fallback requires checkout of the exact workflow commit.')
    try:
        reference = git('rev-parse', '--verify', 'HEAD^1^{commit}', cwd=cwd)
    except subprocess.CalledProcessError as error:
        raise ValueError('Manual/tag native qualification requires a first-parent reference commit.') from error
    if not valid_sha(reference):
        raise ValueError('Native reference fallback did not resolve a concrete commit.')
    return reference


if __name__ == '__main__':
    try:
        print(select_reference(os.environ.get('EVENT_REFERENCE_SHA', ''), os.environ['GITHUB_SHA']))
    except (ValueError, subprocess.CalledProcessError) as error:
        raise SystemExit(f'Native reference selection failed: {error}') from error
