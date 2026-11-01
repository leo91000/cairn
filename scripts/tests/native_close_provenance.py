"""Verify pinned diagnostic sources; fetch ELF only on the disposable CI VM."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import urllib.error
import urllib.request
import zipfile


def git(*args):
    return subprocess.check_output(['git', *args])


def source_proof(manifest):
    refs = manifest['blobs']
    result = {}
    for revision, expected in refs.items():
        assert git('rev-parse', revision).decode().strip() == revision
        observed = {}
        for path, blob in expected.items():
            assert git('rev-parse', revision + ':' + path).decode().strip() == blob
            source = git('show', revision + ':' + path)
            # #645 changed tests only; compare the real close implementation.
            production = source.split(b'#[cfg(test)]\nmod tests', 1)[0]
            observed[path] = dict(blob=blob, productionSha256=hashlib.sha256(production).hexdigest())
        service = git('show', revision + ':crates/installation/src/native/service.rs')
        start = service.index(b'            // Reject in-flight file mutations before flushing/unmounting.')
        end_marker = b'            controller.leases.lock().await.remove(&id);'
        end = service.index(end_marker, start) + len(end_marker)
        observed['controllerFinalization'] = dict(sha256=hashlib.sha256(service[start:end]).hexdigest())
        result[revision] = observed
    deployed = result[manifest['deployedRuntime']]
    for observed in result.values():
        for path in manifest['productionFiles']:
            assert observed[path]['productionSha256'] == deployed[path]['productionSha256'], path
        assert observed['controllerFinalization'] == deployed['controllerFinalization']
    # Keep the synthetic private protocol fixture paired with the reused helper.
    for path in ('crates/installation/examples/native_storage_qualifier.rs',
                 'scripts/tests/native_chat_fixture.py', 'deploy/nodes/native-host.py'):
        assert git('rev-parse', manifest['runtimeSource'] + ':' + path) == git(
            'rev-parse', manifest['sourceBase'] + ':' + path), path
    return result


def request_api(repository, path):
    request = urllib.request.Request('https://api.github.com/repos/' + repository + path,
        headers={'Authorization': 'Bearer ' + os.environ['GITHUB_TOKEN'],
                 'Accept': 'application/vnd.github+json', 'X-GitHub-Api-Version': '2022-11-28'})
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.load(response)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *_args, **_kwargs):
        return None


def fetch_archive(repository, artifact, destination):
    # Obtain the signed artifact URL without forwarding the GitHub bearer token
    # to blob storage. Never print the signed URL or an exception containing it.
    request = urllib.request.Request(
        f'https://api.github.com/repos/{repository}/actions/artifacts/{artifact["id"]}/zip',
        headers={'Authorization': 'Bearer ' + os.environ['GITHUB_TOKEN'],
                 'Accept': 'application/vnd.github+json', 'X-GitHub-Api-Version': '2022-11-28'})
    try:
        urllib.request.build_opener(NoRedirect).open(request, timeout=30)
    except urllib.error.HTTPError as response:
        assert response.code == 302
        location = response.headers['Location']
    else:
        raise AssertionError('Expected the GitHub artifact storage redirect.')
    assert location.startswith('https://')
    checksum = hashlib.sha256()
    total = 0
    with urllib.request.urlopen(location, timeout=60) as response, destination.open('wb') as output:
        while chunk := response.read(1024 * 1024):
            total += len(chunk)
            assert total <= artifact['size'], 'Artifact exceeds the frozen size.'
            checksum.update(chunk)
            output.write(chunk)
    assert total == artifact['size']
    assert checksum.hexdigest() == artifact['digest'], 'Artifact ZIP digest mismatch.'


def binary_proof(manifest, directory):
    repository = manifest['repository']
    run = request_api(repository, f'/actions/runs/{manifest["runId"]}')
    assert run['head_sha'] == manifest['buildWorkflowHead']
    assert run['event'] == 'workflow_dispatch'
    build = request_api(repository, f'/actions/jobs/{manifest["buildJobId"]}')
    assert build['run_id'] == manifest['runId'] and build['conclusion'] == 'success'
    qualification = request_api(repository, f'/actions/jobs/{manifest["qualificationJobId"]}')
    assert qualification['run_id'] == manifest['runId']
    reference_steps = [step for step in qualification['steps']
                       if step['name'] == 'Qualify the exact reference on the current disposable host']
    assert len(reference_steps) == 1 and reference_steps[0]['conclusion'] == 'success'
    artifact = manifest['artifact']
    observed = request_api(repository, f'/actions/artifacts/{artifact["id"]}')
    assert not observed['expired']
    assert observed['name'] == artifact['name']
    assert observed['size_in_bytes'] == artifact['size']
    assert observed['digest'] == 'sha256:' + artifact['digest']
    assert observed['workflow_run']['id'] == manifest['runId']
    assert observed['workflow_run']['head_sha'] == manifest['buildWorkflowHead']
    assert os.environ['ImageOS'] == manifest['build']['imageOS']
    assert os.environ['ImageVersion'] == manifest['build']['imageVersion']
    assert os.environ['RUNNER_ARCH'] == manifest['build']['architecture']
    archive = directory / 'reference.zip'
    fetch_archive(repository, artifact, archive)
    expected = {'cairn', 'examples/native_storage_qualifier', 'revision.json'}
    with zipfile.ZipFile(archive) as zipped:
        entries = zipped.infolist()
        names = [entry.filename for entry in entries]
        assert len(names) == len(set(names)), 'Duplicate ZIP member.'
        for entry in entries:
            kind = stat.S_IFMT(entry.external_attr >> 16)
            if entry.is_dir():
                assert entry.filename == 'examples/' and kind in (0, stat.S_IFDIR)
            else:
                assert entry.filename in expected and kind in (0, stat.S_IFREG)
                assert not entry.external_attr & 0x10, 'Unexpected DOS directory member.'
            assert not entry.flag_bits & 1, 'Encrypted ZIP member.'
        files = [entry for entry in entries if not entry.is_dir()]
        assert len(files) == len(expected) and {entry.filename for entry in files} == expected
        assert sum(entry.file_size for entry in files) <= 1024 * 1024 * 1024
        for entry in files:
            target = directory / entry.filename
            target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            with zipped.open(entry) as source, target.open('wb') as output:
                while chunk := source.read(1024 * 1024):
                    output.write(chunk)
            target.chmod(0o600)
    archive.unlink()
    revision = json.loads((directory / 'revision.json').read_text())
    assert revision['headSha'] == manifest['runtimeSource']
    for key in ('rust', 'features', 'profile', 'buildRun'):
        assert revision[key] == manifest['build'][key], key
    assert revision['image'] == manifest['build']['imageVersion']
    assert set(revision['sha256']) == expected - {'revision.json'}
    assert revision['sha256'] == manifest['binarySha256']
    for name, checksum in revision['sha256'].items():
        assert len(checksum) == 64 and hashlib.sha256((directory / name).read_bytes()).hexdigest() == checksum
        (directory / name).chmod(0o700)
    return dict(artifact=artifact, revision=revision, buildJob=build['id'],
                referenceQualificationStep='success', runtimeSourceIsDeployedBinary=False)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--sources-only', action='store_true')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    manifest = json.loads(Path(__file__).with_suffix('.json').read_text())
    result = dict(schemaVersion=1, expectedResult=manifest['expectedResult'],
                  fixtureHead=git('rev-parse', 'HEAD').decode().strip(),
                  sourceProof=source_proof(manifest), runtimeSource=manifest['runtimeSource'])
    args.output.mkdir(mode=0o700, parents=True, exist_ok=True)
    if not args.sources_only:
        assert os.environ.get('GITHUB_ACTIONS') == 'true'
        assert os.environ.get('RUNNER_ENVIRONMENT') == 'github-hosted'
        assert os.environ.get('GITHUB_REPOSITORY') == manifest['repository']
        assert os.environ.get('CAIRN_NATIVE_DISPOSABLE_HOST') == '1'
        result['binaryProof'] = binary_proof(manifest, args.output)
    path = args.output / 'provenance.json'
    path.write_text(json.dumps(result, indent=2) + '\n')
    path.chmod(0o600)
    print('PASS: pinned diagnostic ' + ('source' if args.sources_only else 'source and binary') + ' provenance')


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        # URL errors may contain a signed download URL. Preserve only its class.
        raise SystemExit('Diagnostic provenance verification failed: ' + type(error).__name__)
