"""Add the same insertion-only observation to both pinned native fixtures."""
import argparse
import ast
import hashlib
import json
from pathlib import Path
import shutil
import symtable


OBSERVER = '_cairn_ci_phase_observer'


def scopes(table):
    yield table
    for child in table.get_children():
        yield from scopes(child)


def instrument(source, role):
    if role not in ('reference', 'candidate') or 'native_phase_diagnostics' in source:
        raise ValueError('Invalid or already observed native fixture')
    if any(name in scope.get_identifiers() for scope in scopes(symtable.symtable(source, '<native-fixture>', 'exec'))
           for name in (OBSERVER, 'start_diagnostics')):
        raise ValueError('Native fixture uses a reserved diagnostic binding')
    resume_anchor = "            resumed = start(run, measure='nativeEvicted')\n"
    if resume_anchor not in source:
        resume_anchor = "            resumed = start(run, measure='nativeLocalRestart')\n"
    insertions = [
        ("    binary = Path(sys.argv[1]).resolve(strict=True)\n",
         "    from native_phase_diagnostics import start as start_diagnostics\n    " + OBSERVER + " = start_diagnostics(" + repr(role) + ")\n"),
        ("    def progress(phase):\n", "        " + OBSERVER + ".enter(phase)\n"),
        ("            progress('attempt-ready')\n", "            " + OBSERVER + ".watch(supervisor, attempt)\n"),
        ("            started_runs.add(run)\n", "            progress('namespace-check-before')\n"),
        ("                assert api('/runs/' + attempt + '/artifact', dict(runId=run, path='/tmp/native-artifact'), raw=True) == b'temporary-artifact'\n",
         "                progress('artifact-check-complete')\n"),
        ("                before = manager('metrics')['remoteRequests']\n", "                progress('restored-measure-trigger')\n"),
        ("                eventually(lambda: (managed(run) / 'measure-ready').exists())\n", "                progress('restored-measure-read')\n"),
        (resume_anchor, "            progress('post-local-resume-complete')\n"),
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
    main = next(scope for scope in symtable.symtable(observed, '<native-fixture>', 'exec').get_children() if scope.get_name() == 'main')
    for name in ('progress', 'start'):
        nested = [scope for scope in main.get_children() if scope.get_name() == name]
        if len(nested) != 1 or not nested[0].lookup(OBSERVER).is_free():
            raise ValueError('Native diagnostic observer must close over its main binding')
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
