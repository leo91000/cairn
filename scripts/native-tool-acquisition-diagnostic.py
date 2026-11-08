# Disposable CI observation only: prefixes do not qualify an artifact or a native runtime.
import argparse
import ast
import contextlib
import hashlib
import json
import multiprocessing
import queue
import re
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

SOURCES = {
    'incident-main': '272f4f3bbe5b276b0b9339cf38bab53b04bef8b2',
    'failed-repair-reference': 'ca526fc46e8aae81c0955a2dd6e116a9bac45d60',
    'repair-candidate': 'c7dc7fb78aeb9f50fdf0e8ed8295659dec6e9b93',
    'current-main': 'c186d11e048cdcb8d2d1ae31a11a277a03411baf',
}
HEADERS = ('server', 'via', 'x-cache', 'age', 'content-type', 'content-length',
           'x-ratelimit-limit', 'x-ratelimit-remaining', 'x-ratelimit-reset')
PROFILES = (
    ('native-default', {}),
    ('cache-revalidate', {'Cache-Control': 'no-cache', 'Pragma': 'no-cache'}),
    ('curl-user-agent', {'User-Agent': 'curl/8.5.0'}),
)


# Asset identities/digests independently observed in official release metadata.
API_ASSETS = {
    'https://github.com/rootless-containers/rootlesskit/releases/download/v3.3.0/rootlesskit-x86_64.tar.gz':
        ('https://api.github.com/repos/rootless-containers/rootlesskit/releases/assets/638878224',
         '0e757c8f80510d764df8c9a5f169805580b48d02329f0a200900e261ecec27ed'),
    'https://github.com/rootless-containers/slirp4netns/releases/download/v1.3.6/slirp4netns-x86_64':
        ('https://api.github.com/repos/rootless-containers/slirp4netns/releases/assets/606623788',
         '411167abceff1fc8a7c562f53ed9f26d3ec8a17df4e2fe6eaa3bd53f1c1e1d48'),
    'https://github.com/docker/compose/releases/download/v5.6.1/docker-compose-linux-x86_64':
        ('https://api.github.com/repos/docker/compose/releases/assets/661714739',
         '47737f4236e6d3ccea7291995203f8f018058783b4c3a9469119811a0b5db8c0'),
    'https://github.com/docker/buildx/releases/download/v0.38.1/buildx-v0.38.1.linux-amd64':
        ('https://api.github.com/repos/docker/buildx/releases/assets/660618355',
         'e25d5cd675092bb5d1d03632fdc46cdf3fede7b514dc5f023a105e00d3ce5ad4'),
    'https://github.com/cargo-bins/cargo-binstall/releases/download/v1.25.3/cargo-binstall-x86_64-unknown-linux-musl.tgz':
        ('https://api.github.com/repos/cargo-bins/cargo-binstall/releases/assets/628279880',
         '1f72931713e9091478538c6c78c3f11f205b4f6f394f35e61835036b23929d2a'),
    'https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-0.9.149/cargo-nextest-0.9.149-x86_64-unknown-linux-gnu.tar.gz':
        ('https://api.github.com/repos/nextest-rs/nextest/releases/assets/660814555',
         '32d1d8c0567b7a80bee2bad8bb09125c846da96bc009b2de11ec08557290c770'),
}


def bindings():
    output = {}
    tools = None
    for role, revision in SOURCES.items():
        raw = subprocess.check_output(['git', 'show', revision + ':deploy/nodes/native-host.py'])
        assignment = next(node for node in ast.parse(raw).body
                          if isinstance(node, ast.Assign)
                          and any(isinstance(target, ast.Name) and target.id == 'DOCKER_TOOLS'
                                  for target in node.targets))
        pinned = [(ast.literal_eval(item.elts[0]), ast.literal_eval(item.elts[1]))
                  for item in assignment.value.elts]
        assert len(pinned) == 5
        for url, digest in pinned:
            parsed = urllib.parse.urlsplit(url)
            assert parsed.scheme == 'https' and not parsed.query and not parsed.fragment
            assert parsed.hostname in ('download.docker.com', 'github.com')
            assert re.fullmatch('[0-9a-f]{64}', digest)
        assert tools is None or tools == pinned
        tools = pinned
        output[role] = {'commit': revision, 'nativeHostSha256': hashlib.sha256(raw).hexdigest()}
    workflow = subprocess.check_output(['git', 'show', SOURCES['current-main'] + ':.github/workflows/ci.yaml'])
    assert b'cargo-nextest@0.9.149' in workflow
    return output, tools


def safe_location(url):
    parsed = urllib.parse.urlsplit(url)
    return urllib.parse.urlunsplit((parsed.scheme, parsed.hostname or '', parsed.path, '', ''))


def observe(url, headers, results):
    started = time.monotonic()
    result = {'status': None, 'errorKind': None}
    try:
        try:
            response = urllib.request.urlopen(urllib.request.Request(url, headers=headers), timeout=18)
        except urllib.error.HTTPError as error:
            response = error
            result['errorKind'] = 'http-error'
        with contextlib.closing(response):
            result['status'] = response.code
            result['finalLocationWithoutQuery'] = safe_location(response.geturl())
            result['headers'] = {name: response.headers[name][:256] for name in HEADERS
                                 if response.headers.get(name) is not None}
            prefix = response.read(1024)
            result['prefixBytes'] = len(prefix)
            result['prefixSha256'] = hashlib.sha256(prefix).hexdigest()
    except Exception as error:
        result['errorKind'] = type(error).__name__
    result['elapsedSeconds'] = round(time.monotonic() - started, 3)
    results.put(result)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--output', required=True)
    parser.add_argument('--dry-run', action='store_true')
    parser.add_argument('--official-routes', action='store_true')
    args = parser.parse_args()
    sources, tools = bindings()
    urls = [url for url, _ in tools] + [
        'https://github.com/cargo-bins/cargo-binstall/releases/download/v1.25.3/cargo-binstall-x86_64-unknown-linux-musl.tgz',
        'https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-0.9.149/cargo-nextest-0.9.149-x86_64-unknown-linux-gnu.tar.gz',
    ]
    requests = [(url, profile, headers) for url in urls for profile, headers in PROFILES]
    if args.official_routes:
        pins = dict(tools)
        for url, (_, digest) in API_ASSETS.items():
            assert url not in pins or pins[url] == digest
        requests = []
        for canonical, (api, _) in API_ASSETS.items():
            requests.append((canonical + '?download=1', 'canonical-download-query', {}))
            requests.append((api, 'official-asset-api', {
                'Accept': 'application/octet-stream', 'X-GitHub-Api-Version': '2022-11-28',
            }))
    report = {'purpose': 'bounded-public-acquisition-diagnostic', 'qualified': False,
              'sourceBindings': sources, 'nativePins': tools, 'observations': [],
              'officialAssetIdentities': API_ASSETS if args.official_routes else {},
              'requestPlan': [{'url': url, 'profile': profile} for url, profile, _ in requests]}
    output = Path(args.output)
    output.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({'sourceBindings': sources, 'qualified': False}), flush=True)
    if args.dry_run:
        return 0
    deadline = time.monotonic() + 480
    context = multiprocessing.get_context('spawn')
    for url, profile, headers in requests:
        remaining = deadline - time.monotonic()
        if remaining <= 2:
            report['diagnosticDeadlineReached'] = True
            output.write_text(json.dumps(report, indent=2) + '\n')
            return 1
        results = context.Queue(maxsize=1)
        process = context.Process(target=observe, args=(url, headers, results))
        process.start()
        process.join(min(22, remaining - 1))
        if process.is_alive():
            process.kill()
            process.join()
            observation = {'status': None, 'errorKind': 'probe-deadline'}
        else:
            try:
                observation = results.get(timeout=0.5)
            except queue.Empty:
                observation = {'status': None, 'errorKind': 'probe-process-exit'}
        results.close()
        results.join_thread()
        observation.update({'url': url, 'profile': profile})
        report['observations'].append(observation)
        output.write_text(json.dumps(report, indent=2) + '\n')
        print(json.dumps(observation), flush=True)
    return 0 if all(item['status'] == 200 for item in report['observations']
                    if args.official_routes or item['profile'] == 'native-default') else 1


if __name__ == '__main__':
    raise SystemExit(main())
