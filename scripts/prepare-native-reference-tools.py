# Prepare an exact, private native tool prerequisite on a disposable CI runner; no daemon.
import argparse
import ast
import hashlib
import importlib.util
import json
import os
import subprocess
import re
import shutil
import tempfile
import time
import traceback
from pathlib import Path



def source(revision):
    return subprocess.check_output(['git', 'show', revision + ':deploy/nodes/native-host.py'])


def assignment(raw, name):
    return next(node.value for node in ast.parse(raw).body if isinstance(node, ast.Assign)
                and any(isinstance(target, ast.Name) and target.id == name for target in node.targets))


def pins(raw):
    return [(ast.literal_eval(item.elts[0]), ast.literal_eval(item.elts[1]))
            for item in assignment(raw, 'DOCKER_TOOLS').elts]



def definitions(raw):
    # The pinned declaration includes one bounded literal dict comprehension.
    # No calls, attributes, subscriptions, operators other than string addition,
    # or access to builtins are admitted when resolving this source data.
    node = assignment(raw, 'DOCKER_TOOLS')
    allowed = (ast.Tuple, ast.List, ast.Dict, ast.DictComp, ast.comprehension,
               ast.Constant, ast.BinOp, ast.Add, ast.Name, ast.Load, ast.Store)
    if any(not isinstance(item, allowed) for item in ast.walk(node)):
        raise ValueError('Unknown pinned Docker definition shape.')
    return eval(compile(ast.Expression(body=node), '<pinned-docker-definitions>', 'eval'), {'__builtins__': {}})


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--output', required=True)
    parser.add_argument('--dry-run', action='store_true')
    parser.add_argument('--candidate', required=True)
    parser.add_argument('--reference', required=True)
    parser.add_argument('--cache-output', type=Path)
    args = parser.parse_args()
    if not all(re.fullmatch(r'[0-9a-f]{40}', value) and value != '0' * 40
               for value in (args.candidate, args.reference)):
        parser.error('Source identities must be nonzero full commit SHAs.')
    if args.cache_output and (args.cache_output.exists() or args.cache_output.is_symlink()):
        parser.error('Only a fresh candidate cache may be published.')
    selected = args.candidate
    raw, baseline = source(selected), source(args.reference)
    same_definitions = definitions(raw) == definitions(baseline)
    same_wrapper = ast.literal_eval(assignment(raw, 'DOCKER_WRAPPER')) == ast.literal_eval(assignment(baseline, 'DOCKER_WRAPPER'))
    eligible = same_definitions and same_wrapper
    report = {'source': selected, 'role': 'candidate', 'candidate': args.candidate, 'reference': args.reference,
              'nativeHostSha256': hashlib.sha256(raw).hexdigest(),
              'canonicalPinsUnchanged': same_definitions, 'wrapperUnchanged': same_wrapper,
              'referenceCacheEligible': eligible, 'cachePublished': False,
              'condition': 'warmReferenceColdCandidate' if eligible else 'coldReferenceColdCandidate',
              'pins': pins(raw),
              'toolDefinitions': definitions(raw),
              'wrapperSha256': hashlib.sha256(ast.literal_eval(assignment(raw, 'DOCKER_WRAPPER')).encode()).hexdigest(), 'acquisitions': [], 'acquisitionQualified': False,
              'physicalRuntimeQualified': False}
    output = Path(args.output).resolve()
    def record():
        output.write_text(json.dumps(report, indent=2) + '\n')
    record()
    if args.dry_run or not eligible:
        print(json.dumps(report), flush=True)
        return 0
    if os.environ.get('GITHUB_ACTIONS') != 'true' or os.environ.get('RUNNER_ENVIRONMENT') != 'github-hosted':
        raise RuntimeError('Acquisition setup runs only on the disposable hosted CI runner.')
    started = time.monotonic()
    try:
        with tempfile.TemporaryDirectory(prefix='cairn-np-', dir='/var/tmp') as directory:
            root = Path(directory)
            module_file = root / 'native-host.py'
            module_file.write_bytes(raw)
            spec = importlib.util.spec_from_file_location('qualified_native_tool_preparation', module_file)
            host = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(host)
            host.INSTALL = root / 'i'
            legacy_opener = not hasattr(host, 'open_docker_tool')
            report['legacyDirectOpener'] = legacy_opener
            original_open = host.urllib.request.urlopen if legacy_opener else host.open_docker_tool
            allowed = dict(pins(raw))
            def observe_open(url, *open_args, **open_kwargs):
                assert url in allowed
                event = {'canonicalUrl': url, 'expectedSha256': allowed[url], 'opened': False}
                report['acquisitions'].append(event)
                record()
                print(json.dumps({'phase': 'acquire', **event}), flush=True)
                response = original_open(url, *open_args, **open_kwargs)
                event['opened'] = True
                event['status'] = response.code
                record()
                return response
            # Observation only: preserve the exact opener, response and all checks.
            if legacy_opener:
                host.urllib.request.urlopen = observe_open
            else:
                host.open_docker_tool = observe_open
            original_probe = host.subprocess.run
            def observe_probe(command, **kwargs):
                assert command[-3:] == ['version', '--format', '{{json .Client}}']
                assert set(kwargs['env']) == {'PATH', 'HOME', 'DOCKER_CONFIG', 'DOCKER_HOST'}
                socket_path = kwargs['env']['DOCKER_HOST'].removeprefix('unix://')
                report['offlineSocketBytes'] = len(socket_path.encode())
                record()
                assert report['offlineSocketBytes'] < 108, 'The private probe socket must fit Linux sun_path'
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
            report['completePinnedPayloadsVerifiedBy'] = 'prepare_docker_tools@' + selected
            if args.cache_output:
                shutil.copytree(target, args.cache_output, copy_function=shutil.copy2)
                report['cachePublished'] = True
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
