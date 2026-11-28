"""Select an immutable comparison commit for every supported CI event."""
import os
import json
import re
import subprocess
import sys
import urllib.request


def current_run_artifact(name, environment=None, opener=urllib.request.urlopen):
    environment = os.environ if environment is None else environment
    repository, run = environment['GITHUB_REPOSITORY'], environment['GITHUB_RUN_ID']
    if not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', repository) or not run.isdecimal():
        raise ValueError('Artifact reuse requires the current repository and numeric workflow run.')
    page = 1
    while True:
        # Scope reuse to this exact run (and therefore candidate commit), never
        # another PR's artifact. The name binds reference SHA, image and profile;
        # the workflow verifies the embedded metadata and binary checksums too.
        request = urllib.request.Request(
            f'https://api.github.com/repos/{repository}/actions/runs/{run}/artifacts?per_page=100&page={page}',
            headers={'Authorization': 'Bearer ' + environment['GITHUB_TOKEN'], 'Accept': 'application/vnd.github+json'},
        )
        with opener(request, timeout=30) as response:
            artifacts = json.load(response)['artifacts']
        if any(artifact['name'] == name and artifact['expired'] is False for artifact in artifacts):
            return True
        if len(artifacts) < 100:
            return False
        page += 1


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
        if sys.argv[1:] == ['artifact-available']:
            print(str(current_run_artifact(os.environ['NATIVE_REFERENCE_ARTIFACT'])).lower())
        elif sys.argv[1:]:
            raise ValueError('Unknown native reference operation.')
        else:
            print(select_reference(os.environ.get('EVENT_REFERENCE_SHA', ''), os.environ['GITHUB_SHA']))
    except (ValueError, subprocess.CalledProcessError) as error:
        raise SystemExit(f'Native reference selection failed: {error}') from error
