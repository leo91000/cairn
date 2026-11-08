# Disposable, unprivileged prerequisite qualification; no daemon, native workload or release.
import argparse
import ast
import hashlib
import importlib.util
import json
import subprocess
import tempfile
import time
import traceback
from pathlib import Path

CANDIDATE = '35d151a14a912ff5436ae152813c66321a6bb469'
REFERENCE = 'c186d11e048cdcb8d2d1ae31a11a277a03411baf'


def source(revision):
    return subprocess.check_output(['git', 'show', revision + ':deploy/nodes/native-host.py'])


def assignment(raw, name):
    return next(node.value for node in ast.parse(raw).body if isinstance(node, ast.Assign)
                and any(isinstance(target, ast.Name) and target.id == name for target in node.targets))


def pins(raw):
    return [(ast.literal_eval(item.elts[0]), ast.literal_eval(item.elts[1]))
            for item in assignment(raw, 'DOCKER_TOOLS').elts]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--output', required=True)
    parser.add_argument('--dry-run', action='store_true')
    args = parser.parse_args()
    raw, baseline = source(CANDIDATE), source(REFERENCE)
    assert pins(raw) == pins(baseline)
    assert ast.literal_eval(assignment(raw, 'DOCKER_WRAPPER')) == ast.literal_eval(assignment(baseline, 'DOCKER_WRAPPER'))
    report = {'candidate': CANDIDATE, 'reference': REFERENCE,
              'nativeHostSha256': hashlib.sha256(raw).hexdigest(),
              'canonicalPinsUnchanged': True, 'wrapperUnchanged': True,
              'pins': pins(raw), 'acquisitions': [], 'acquisitionQualified': False,
              'physicalRuntimeQualified': False}
    output = Path(args.output).resolve()
    def record():
        output.write_text(json.dumps(report, indent=2) + '\n')
    record()
    if args.dry_run:
        print(json.dumps(report), flush=True)
        return 0
    started = time.monotonic()
    try:
        with tempfile.TemporaryDirectory(prefix='native-tool-preparation-', dir=Path.cwd()) as directory:
            root = Path(directory)
            module_file = root / 'native-host.py'
            module_file.write_bytes(raw)
            spec = importlib.util.spec_from_file_location('qualified_native_tool_preparation', module_file)
            host = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(host)
            host.INSTALL = root / 'install'
            original_open = host.open_docker_tool
            allowed = dict(pins(raw))
            def observe_open(url):
                assert url in allowed
                event = {'canonicalUrl': url, 'expectedSha256': allowed[url], 'opened': False}
                report['acquisitions'].append(event)
                record()
                print(json.dumps({'phase': 'acquire', **event}), flush=True)
                response = original_open(url)
                event['opened'] = True
                event['status'] = response.code
                record()
                return response
            # Observation only: preserve the exact opener, response and all checks.
            host.open_docker_tool = observe_open
            original_probe = host.subprocess.run
            def observe_probe(command, **kwargs):
                assert command[-3:] == ['version', '--format', '{{json .Client}}']
                assert set(kwargs['env']) == {'PATH', 'HOME', 'DOCKER_CONFIG', 'DOCKER_HOST'}
                result = original_probe(command, **kwargs)
                report['offlineClientProbe'] = {
                    'returnCode': result.returncode,
                    'stdout': result.stdout[:2048].replace(str(root), '<private-preparation>'),
                    'stderr': result.stderr[:2048].replace(str(root), '<private-preparation>'),
                    'environmentKeys': sorted(kwargs['env']),
                }
                record()
                return result
            host.subprocess.run = observe_probe
            try:
                target = host.prepare_docker_tools()
            finally:
                host.subprocess.run = original_probe
            assert len(report['acquisitions']) == len(allowed)
            manifest = json.loads((target / 'manifest.json').read_text())
            for name, expected in manifest.items():
                path = target / name
                assert not path.is_symlink() and path.is_file()
                assert hashlib.sha256(path.read_bytes()).hexdigest() == expected
            assert host.prepare_docker_tools() == target
            assert len(report['acquisitions']) == len(allowed)
            report['manifest'] = manifest
            report['engineApi'] = json.loads((target / 'engine-api.json').read_text())
            report['cacheReuseVerified'] = True
            report['completePinnedPayloadsVerifiedBy'] = 'prepare_docker_tools@' + CANDIDATE
            report['acquisitionQualified'] = True
    except Exception as error:
        report['acquisitionQualified'] = False
        report['errorFrames'] = [{'file': Path(frame.filename).name, 'line': frame.lineno, 'function': frame.name}
                                 for frame in traceback.extract_tb(error.__traceback__)[-6:]]
        report['errorKind'] = type(error).__name__
        report['httpStatus'] = getattr(error, 'code', None)
    report['elapsedSeconds'] = round(time.monotonic() - started, 3)
    record()
    print(json.dumps(report), flush=True)
    return 0 if report['acquisitionQualified'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
