"""Disposable CI setup for an exact reference with a failing cold download route.

This is fixture input, not an observation-only overlay. The reference retains its
shipped helper/cache checks and every runtime assertion. It is used only in CI.
"""
import argparse
import ast
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import subprocess
import textwrap


ANCHOR = "        host.INSTALL = root / 'managed-tools'\n"


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def verify_cache(cache, preparation, definitions, wrapper):
    if (preparation.get('referenceCacheEligible') is not True
            or preparation.get('acquisitionQualified') is not True
            or preparation.get('physicalRuntimeQualified') is not False
            or preparation.get('cacheReuseVerified') is not True
            or preparation.get('cachePublished') is not True):
        raise ValueError('A successful pristine preparation receipt is required.')
    normalized = json.loads(json.dumps(definitions))
    if (preparation['toolDefinitions'] != normalized
            or preparation['wrapperSha256'] != hashlib.sha256(wrapper.encode()).hexdigest()):
        raise ValueError('Reference tools or wrapper differ from the prepared candidate.')
    names = {'docker', 'engine-api.json'}
    for _, _, entries in definitions:
        names.update(entries.values())
    if len(names) != 13:
        raise ValueError('Unexpected pinned Docker inventory.')
    for name in names:
        if PurePosixPath(name).is_absolute() or '..' in PurePosixPath(name).parts:
            raise ValueError('Invalid Docker inventory path.')
    if cache.resolve(strict=True) != cache.absolute() or cache.is_symlink():
        raise ValueError('Cache path must be direct.')
    paths = list(cache.rglob('*'))
    if any(path.is_symlink() for path in paths):
        raise ValueError('Cache contains a symlink.')
    files = {str(path.relative_to(cache)) for path in paths if path.is_file()}
    directories = {str(path.relative_to(cache)) for path in paths if path.is_dir()}
    if (files != names | {'manifest.json'} or directories != {'bin', 'plugins'}
            or len(paths) != len(files) + len(directories)):
        raise ValueError('Cache contains an unexpected entry.')
    inventory = json.loads((cache / 'manifest.json').read_text())
    if set(inventory) != names or inventory != preparation['manifest']:
        raise ValueError('Cache manifest differs from the qualified inventory.')
    for name, expected in inventory.items():
        path = cache / name
        mode = stat.S_IMODE(path.stat().st_mode)
        wanted_mode = 0o444 if name == 'engine-api.json' else 0o555
        if not re.fullmatch(r'[0-9a-f]{64}', expected) or digest(path) != expected or mode != wanted_mode:
            raise ValueError('Cache payload or mode differs from the qualified inventory.')
    if digest(cache / 'docker') != hashlib.sha256(wrapper.encode()).hexdigest():
        raise ValueError('Cached wrapper bytes differ from the exact reference wrapper.')
    if stat.S_IMODE((cache / 'manifest.json').stat().st_mode) != 0o444:
        raise ValueError('Cache manifest must be read-only.')
    return {name: digest(cache / name) for name in sorted(files)}


def seed_reference(host, root, cache, preparation_path, receipt_path, expected_host_sha):
    if (os.geteuid() != 0 or os.environ.get('CAIRN_NATIVE_DISPOSABLE_HOST') != '1'
            or not os.environ.get('CAIRN_NODE_TEST_S3_ENDPOINT', '').startswith('http://127.0.0.1:')):
        raise RuntimeError('Reference setup requires the explicitly disposable loopback-S3 fixture.')
    if digest(Path(host.__file__)) != expected_host_sha or host.INSTALL != root / 'managed-tools':
        raise ValueError('Reference helper or private installation differs from the pinned fixture.')
    if root.is_symlink() or root.resolve(strict=True) != root.absolute() or root.stat().st_uid != 0:
        raise ValueError('Reference fixture root must be private and root-owned.')
    preparation = json.loads(preparation_path.read_text())
    inventory = verify_cache(cache, preparation, host.DOCKER_TOOLS, host.DOCKER_WRAPPER)
    target = host.INSTALL / 'docker-tools/v2'
    if host.INSTALL.exists() or host.INSTALL.is_symlink():
        raise ValueError('Reference installation must be fresh before cache setup.')
    target.parent.mkdir(parents=True, mode=0o755)
    shutil.copytree(cache, target, copy_function=shutil.copy2)
    copied = verify_cache(target, preparation, host.DOCKER_TOOLS, host.DOCKER_WRAPPER)
    if copied != inventory:
        raise ValueError('Reference cache copy changed.')
    for name in inventory:
        before, after = (cache / name).stat(), (target / name).stat()
        if before.st_dev == after.st_dev and before.st_ino == after.st_ino:
            raise ValueError('Reference cache must have independent inodes.')
    if verify_cache(cache, preparation, host.DOCKER_TOOLS, host.DOCKER_WRAPPER) != inventory:
        raise ValueError('Pristine cache changed while copying.')
    receipt_path.write_text(json.dumps(dict(
        condition='warmReferenceColdCandidate', referenceHostSha256=expected_host_sha,
        candidate=preparation['candidate'], reference=preparation['reference'],
        copiedFileSha256=copied, fileCount=len(copied), independentInodes=True,
        coldReferenceAcquisitionQualified=False, referenceRuntimeQualified=False,
        candidateRuntimeQualified=False,
    ), indent=2) + '\n')


def instrument(source, insertion):
    if source.count(ANCHOR) != 1 or 'from native_reference_tool_cache import' in source:
        raise ValueError('Unknown reference fixture setup shape.')
    result = source.replace(ANCHOR, ANCHOR + insertion, 1)
    if result.replace(insertion, '', 1) != source:
        raise ValueError('Reference setup must be insertion-only.')
    before, after = ast.parse(source), ast.parse(result)
    functions = [node for node in ast.walk(after) if isinstance(node, ast.FunctionDef) and node.name == 'main']
    if len(functions) != 1:
        raise ValueError('Reference fixture main is ambiguous.')
    blocks = [node for node in ast.walk(functions[0]) if isinstance(node, ast.With)
              and any(isinstance(child, ast.ImportFrom) and child.module == 'native_reference_tool_cache'
                      for child in node.body)]
    if len(blocks) != 1:
        raise ValueError('Reference setup insertion is ambiguous.')
    block = blocks[0]
    indexes = [index for index, node in enumerate(block.body)
               if isinstance(node, ast.ImportFrom) and node.module == 'native_reference_tool_cache']
    index = indexes[0]
    expected = ast.parse(textwrap.dedent(insertion)).body
    if ast.dump(ast.Module(body=block.body[index:index + 2], type_ignores=[])) != ast.dump(ast.Module(body=expected, type_ignores=[])):
        raise ValueError('Reference setup call differs from its closed contract.')
    del block.body[index:index + 2]
    if ast.dump(before) != ast.dump(after):
        raise ValueError('Reference setup changed existing fixture behavior.')
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('fixture', type=Path)
    parser.add_argument('--reference', required=True)
    parser.add_argument('--cache', type=Path, required=True)
    parser.add_argument('--preparation', type=Path, required=True)
    parser.add_argument('--receipt', type=Path, required=True)
    args = parser.parse_args()
    if not re.fullmatch(r'[0-9a-f]{40}', args.reference):
        parser.error('A full reference SHA is required.')
    target = args.fixture / 'scripts/tests/native_runtime_test.py'
    original = subprocess.check_output(['git', 'show', args.reference + ':scripts/tests/native_runtime_test.py'])
    if target.read_bytes() != original:
        raise ValueError('Reference fixture must match its archived source before setup.')
    raw = subprocess.check_output(['git', 'show', args.reference + ':deploy/nodes/native-host.py'])
    host_file = args.fixture / 'deploy/nodes/native-host.py'
    if host_file.read_bytes() != raw:
        raise ValueError('Reference helper must match its source.')
    preparation = json.loads(args.preparation.read_text())
    if preparation.get('reference') != args.reference:
        raise ValueError('Prepared cache belongs to another reference pair.')
    if preparation.get('referenceCacheEligible') is False:
        if preparation.get('condition') != 'coldReferenceColdCandidate' or preparation.get('cachePublished') is not False:
            raise ValueError('Invalid distinct-tool preparation receipt.')
        print(json.dumps(dict(condition='coldReferenceColdCandidate', reference=args.reference,
                              referenceFixtureChanged=False, candidateFixtureChanged=False,
                              qualificationClaim=False)))
        return
    host_sha = hashlib.sha256(raw).hexdigest()
    insertion = ('        from native_reference_tool_cache import seed_reference\n'
                 '        seed_reference(host, root, Path(' + repr(str(args.cache.resolve(strict=True))) + '), Path('
                 + repr(str(args.preparation.resolve(strict=True))) + '), Path('
                 + repr(str(args.receipt.absolute())) + '), ' + repr(host_sha) + ')\n')
    result = instrument(original.decode(), insertion)
    target.write_text(result)
    shutil.copyfile(__file__, target.with_name('native_reference_tool_cache.py'))
    print(json.dumps(dict(condition='warmReferenceColdCandidate', reference=args.reference,
                          originalFixtureSha256=hashlib.sha256(original).hexdigest(),
                          preparedFixtureSha256=hashlib.sha256(result.encode()).hexdigest(),
                          referenceHostSha256=host_sha, insertionOnly=True, assertionChanges=False,
                          candidateFixtureChanged=False, qualificationClaim=False)))


if __name__ == '__main__':
    main()
