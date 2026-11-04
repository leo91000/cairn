"""Add the same insertion-only observation to both pinned native fixtures."""
import argparse
import ast
import hashlib
import json
from pathlib import Path
import shutil


def instrument(source, role):
    if role not in ('reference', 'candidate') or 'native_phase_diagnostics' in source:
        raise ValueError('Invalid or already observed native fixture')
    insertions = [
        ("    binary = Path(sys.argv[1]).resolve(strict=True)\n",
         "    from native_phase_diagnostics import start as start_diagnostics\n    diagnostics = start_diagnostics(" + repr(role) + ")\n"),
        ("    def progress(phase):\n", "        diagnostics.enter(phase)\n"),
        ("            progress('attempt-ready')\n", "            diagnostics.watch(supervisor, attempt)\n"),
        ("            started_runs.add(run)\n", "            progress('namespace-check-before')\n"),
        ("                assert api('/runs/' + attempt + '/artifact', dict(runId=run, path='/tmp/native-artifact'), raw=True) == b'temporary-artifact'\n",
         "                progress('artifact-check-complete')\n"),
        ("                before = manager('metrics')['remoteRequests']\n", "                progress('restored-measure-trigger')\n"),
        ("                eventually(lambda: (managed(run) / 'measure-ready').exists())\n", "                progress('restored-measure-read')\n"),
        ("            resumed = start(run, measure='nativeEvicted')\n", "            progress('post-eviction-resume-complete')\n"),
    ]
    observed = source
    for anchor, addition in insertions:
        if observed.count(anchor) != 1:
            raise ValueError('Native fixture phase shape changed; refusing diagnostic overlay')
        # Imports belong before first binary use; progress is inserted into its
        # body. Every other marker follows an unchanged existing statement.
        if anchor.startswith('    binary ='):
            observed = observed.replace(anchor, addition + anchor, 1)
        else:
            observed = observed.replace(anchor, anchor + addition, 1)
    ast.parse(observed)
    restored = observed
    for _, addition in insertions:
        if restored.count(addition) != 1:
            raise ValueError('Ambiguous native diagnostic insertion')
        restored = restored.replace(addition, '', 1)
    if restored != source or ast.dump(ast.parse(restored), include_attributes=False) != ast.dump(ast.parse(source), include_attributes=False):
        raise ValueError('Native diagnostic overlay changed qualification behavior')
    return observed


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('repo', type=Path)
    parser.add_argument('role', choices=('reference', 'candidate'))
    args = parser.parse_args()
    target = args.repo / 'scripts/tests/native_runtime_test.py'
    source = target.read_text()
    observed = instrument(source, args.role)
    target.write_text(observed)
    shutil.copyfile(Path(__file__).with_name('native_phase_diagnostics.py'), target.with_name('native_phase_diagnostics.py'))
    sha = lambda value: hashlib.sha256(value.encode()).hexdigest()
    print(json.dumps(dict(role=args.role, observer='native-phases', observationOnly=True,
                          originalSha256=sha(source), observedSha256=sha(observed))))


if __name__ == '__main__':
    main()
