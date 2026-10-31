"""CI-only, two actual source revisions; typed SQL only, never a live database.

The existing 1,000-record message/question workloads retain their readers,
writers and timers. A private source patch selects only the typed layout and one
repetition; three alternating trials compare the actual reference/candidate.
"""
from contextlib import closing
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import sqlite3
import subprocess
import sys
import tempfile


def digest(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def run(args, cwd=None, env=None, **kwargs):
    return subprocess.run(args, cwd=cwd, env=env, check=True, **kwargs)


def git(*args, cwd=None):
    return subprocess.check_output(['git', *args], cwd=cwd, text=True).strip()


def host():
    return {'platform': list(platform.uname()), 'cpus': os.cpu_count(),
            'load': list(os.getloadavg()),
            'memory': next(line for line in Path('/proc/meminfo').read_text().splitlines()
                           if line.startswith('MemTotal:'))}


def main():
    if os.environ.get('GITHUB_ACTIONS') != 'true':
        raise RuntimeError('CI only; no local build or measurement is authorized by this runner')
    reference = sys.argv[1]
    if not re.fullmatch('[0-9a-f]{40}', reference):
        raise ValueError('Full reference SHA required')
    repo = Path(__file__).resolve().parents[1]
    candidate = git('rev-parse', 'HEAD', cwd=repo)
    run(['git', 'merge-base', '--is-ancestor', reference, candidate], cwd=repo)
    if git('diff', '--name-only', reference, candidate, '--', 'Cargo.lock', 'vendor', cwd=repo):
        raise RuntimeError('Reference and candidate must use the same locked third-party dependencies')
    output = Path(os.environ['PLAN_STORAGE_OUTPUT']).resolve()
    output.mkdir(parents=True, exist_ok=False)
    work = Path(tempfile.mkdtemp(prefix='plan-storage-', dir=os.environ['RUNNER_TEMP']))
    binaries = work / 'binaries'
    binaries.mkdir()
    target = work / 'target'
    env = dict(os.environ, CARGO_TARGET_DIR=str(target), CARGO_INCREMENTAL='0',
               CARGO_PROFILE_DEV_DEBUG='0', CARGO_PROFILE_TEST_DEBUG='0',
               CARGO_BUILD_JOBS='2', RUST_TEST_THREADS='1')
    manifest = {'candidate': candidate, 'reference': reference, 'hostBefore': host(),
                'profile': {key: env[key] for key in ('CARGO_PROFILE_DEV_DEBUG',
                    'CARGO_PROFILE_TEST_DEBUG', 'CARGO_INCREMENTAL', 'CARGO_BUILD_JOBS', 'RUST_TEST_THREADS')},
                'roles': {}, 'rows': [], 'limits': 'Local CI filesystem, not production/network latency. Cold wall excludes connection-open and is not cold OS/device cache. Default-false writers, not the protected-question extra UPDATE or actor admission.'}
    sources = {}
    original = {}

    def save():
        (output / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')

    def cargo(role, root, arguments, label):
        stdout = output / f'{role}-{label}.jsonl'
        with stdout.open('w') as out, (output / f'{role}-{label}.log').open('w') as err:
            run(['cargo', *arguments, '--message-format=json'], cwd=root, env=env, stdout=out, stderr=err)
        records = [json.loads(line) for line in stdout.read_text().splitlines() if line]
        return [record for record in records if record.get('reason') == 'compiler-artifact']

    try:
        for role, commit in [('reference', reference), ('candidate', candidate)]:
            root = work / role
            run(['git', 'worktree', 'add', '--detach', str(root), commit], cwd=repo)
            sources[role] = root
            assert git('status', '--porcelain', cwd=root) == ''
            version = int(re.search(r'DATABASE_VERSION: i64 = (\d+)', (root / 'crates/installation/src/store.rs').read_text())[1])
            manifest['roles'][role] = {'commit': commit, 'tree': git('rev-parse', 'HEAD^{tree}', cwd=root), 'version': version}
            harness = root / 'crates/installation/tests/project_measurement.rs'
            question = root / 'crates/installation/tests/project_measurement/a8.rs'
            original[role] = {harness: harness.read_bytes(), question: question.read_bytes()}
            text = harness.read_text()
            assert text.count('for repetition in 0..5 {') == 1
            text = text.replace('for repetition in 0..5 {', 'for repetition in 0..1 {')
            match = re.search(r'for reference in if repetition % 2 == 0 \{\s*\[true, false\]\s*\} else \{\s*\[false, true\]\s*\} \{', text)
            if not match:
                raise RuntimeError('The current typed workload loop changed; review the harness before qualification')
            text = text[:match.start()] + 'for reference in [false] {' + text[match.end():]
            harness.write_text(text)
            text = question.read_text()
            anchor = 'impl StorageEntity for MeasuredQuestion {'
            assert text.count(anchor) == 1
            text = text.replace(anchor, anchor + '''
    fn encoded_bytes(db: &Db<'_>) -> Option<i64> {
        Some(db.0.query_row("SELECT coalesce(sum(payload),0) FROM dbstat WHERE name IN (SELECT name FROM sqlite_schema WHERE type IN ('table','index') AND (tbl_name='chat_questions' OR tbl_name LIKE 'question_%'))", [], |row| row.get(0)).unwrap())
    }
''', 1)
            question.write_text(text)
            manifest['roles'][role]['harness'] = {str(path.relative_to(root)): {'original': hashlib.sha256(data).hexdigest(), 'measured': digest(path)} for path, data in original[role].items()}
            # One CI target, one build at a time. Force every workspace crate to
            # rebuild across roots; only identical locked third-party crates are shared.
            metadata = json.loads(subprocess.check_output(['cargo', 'metadata', '--locked', '--no-deps', '--format-version=1'], cwd=root, env=env))
            members = set(metadata['workspace_members'])
            packages = [package['name'] for package in metadata['packages'] if package['id'] in members]
            clean = ['cargo', 'clean', '--target-dir', str(target)]
            for package in packages:
                clean += ['-p', package]
            run(clean, cwd=root, env=env, stdout=subprocess.DEVNULL)
            records = cargo(role, root, ['test', '--locked', '-p', 'cairn-installation', '--test', 'project_measurement', '--no-run'], 'measure-build')
            for name in ['cairn_contracts', 'cairn_installation']:
                built = [record for record in records if record['target']['name'] == name]
                assert built and all(not record['fresh'] and Path(record['target']['src_path']).is_relative_to(root) for record in built), f'Workspace crate not rebuilt at the actual role source: {name}'
            measured = next(record['executable'] for record in records if record['target']['name'] == 'project_measurement' and record.get('executable'))
            paths = {'measurement': measured}
            records = cargo(role, root, ['build', '--locked', '-p', 'cairn-installation', '--bin', 'cairn', '--example', 'browser_fixture'], 'fence-build')
            for name in ['cairn', 'browser_fixture']:
                paths[name] = next(record['executable'] for record in records if record['target']['name'] == name and record.get('executable'))
            manifest['roles'][role]['binaries'] = {}
            for name, path in paths.items():
                copied = binaries / f'{role}-{name}'
                shutil.copy2(path, copied)
                manifest['roles'][role]['binaries'][name] = {'path': str(copied), 'sha256': digest(copied)}
            save()

        assert manifest['roles']['candidate']['version'] == manifest['roles']['reference']['version'] + 1
        for trial in range(3):
            for role in (['reference', 'candidate'] if trial % 2 == 0 else ['candidate', 'reference']):
                binary = manifest['roles'][role]['binaries']['measurement']
                assert digest(binary['path']) == binary['sha256']
                for case in ['message_storage_layouts', 'a8_question_storage_layouts']:
                    log = output / f'{trial}-{role}-{case}.log'
                    with log.open('w') as stream:
                        run([binary['path'], case, '--ignored', '--nocapture', '--test-threads=1'], cwd=sources[role], env=env, stdout=stream, stderr=stream)
                    rows = [json.loads(line.split('DOMAIN_STORAGE ', 1)[1]) for line in log.read_text().splitlines() if 'DOMAIN_STORAGE ' in line]
                    assert len(rows) == 1
                    manifest['rows'].append({'trial': trial, 'role': role, 'case': case, 'measurement': rows[0]})
                    save()
        fence = work / 'owned-fence'
        fence.mkdir()
        database = fence / 'manager.db'
        with closing(sqlite3.connect(database)) as db, db:
            db.executescript((sources['reference'] / 'crates/installation/tests/fixtures/questions-waits-v15.sql').read_text())
        manifest['fence'] = {}
        for role in ['reference', 'candidate']:
            helper = manifest['roles'][role]['binaries']['browser_fixture']
            assert digest(helper['path']) == helper['sha256']
            result = subprocess.run([helper['path']], input=json.dumps({'operation': 'upgradeStoreCopy', 'args': [str(fence)]}) + '\n', cwd=sources[role], env=env, capture_output=True, text=True, check=True)
            value = json.loads(result.stdout)['result']
            assert value['versionAfter'] == manifest['roles'][role]['version']
            manifest['fence'][role] = value
        # SQLite connection context managers commit but do not close. Stabilize
        # this owned synthetic fixture before hashing its main file: otherwise a
        # later reader's close can checkpoint committed WAL pages without making
        # any logical database change. The old CLI must still leave every byte
        # of the checkpointed main file unchanged.
        with closing(sqlite3.connect(database)) as db:
            checkpoint = db.execute('PRAGMA wal_checkpoint(TRUNCATE)').fetchone()
            assert checkpoint == (0, 0, 0), f'Fixture WAL was not fully checkpointed: {checkpoint}'
            version_before = db.execute('PRAGMA user_version').fetchone()[0]
            assert version_before == manifest['roles']['candidate']['version']
            assert db.execute('PRAGMA integrity_check').fetchone()[0] == 'ok'
        before = digest(database)
        manifest['fence']['beforeOldCli'] = {'databaseSha256': before, 'version': version_before, 'checkpoint': list(checkpoint)}
        save()
        home = work / 'old-cli-home'
        home.mkdir()
        old = manifest['roles']['reference']['binaries']['cairn']
        assert digest(old['path']) == old['sha256']
        isolated = {'PATH': env['PATH'], 'HOME': str(home), 'AGENT_HOME': str(home), 'DATA_DIR': str(fence), 'WORKSPACE_ROOTS': str(home), 'WORKER_ENABLED': 'false', 'HOST': '127.0.0.1', 'PORT': '0', 'PUBLIC_URL': 'http://localhost:4310'}
        result = subprocess.run([old['path']], cwd=home, env=isolated, text=True, capture_output=True, timeout=20)
        after = digest(database)
        manifest['fence'].update({'oldCliExit': result.returncode, 'oldCliDiagnostic': result.stderr, 'afterOldCliDatabaseSha256': after})
        save()
        assert result.returncode != 0 and 'Unsupported installation database version' in result.stderr
        assert after == before, 'The old CLI changed the checkpointed candidate database'
        with closing(sqlite3.connect(database)) as db:
            assert db.execute('PRAGMA user_version').fetchone()[0] == manifest['roles']['candidate']['version']
            assert db.execute('PRAGMA integrity_check').fetchone()[0] == 'ok'
        manifest['fence'].update({'oldCliExit': result.returncode, 'oldCliDiagnostic': result.stderr, 'unchangedDatabaseSha256': before})
        manifest['hostAfter'] = host()
        save()
        print('Typed Plan storage qualification: 12 rows and actual old-binary refusal verified; see manifest.json')
    finally:
        for role, files in original.items():
            for path, data in files.items():
                path.write_bytes(data)
            assert git('status', '--porcelain', cwd=sources[role]) == ''
        save()


if __name__ == '__main__':
    main()
