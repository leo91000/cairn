"""Observe an archived reference/current fixture without altering its assertions."""
import argparse
import ast
import hashlib
import json
from pathlib import Path
import shutil


ANCHOR = "                if response.status >= 400:\n"


def instrument(source, role):
    if role not in ('reference', 'candidate'):
        raise ValueError('Unknown diagnostic role.')
    if source.count(ANCHOR) != 1 or 'from native_http_diagnostics import report_rejection' in source:
        raise ValueError('Native fixture API shape changed; refusing to patch.')
    insertion = ("                    from native_http_diagnostics import report_rejection\n"
                 "                    report_rejection(path, method, response.status, payload, " + repr(role) + ")\n")
    before = ast.parse(source)
    after_source = source.replace(ANCHOR, ANCHOR + insertion, 1)
    after = ast.parse(after_source)
    functions = [node for node in ast.walk(after) if isinstance(node, ast.FunctionDef) and node.name == 'api']
    if len(functions) != 1:
        raise ValueError('Native fixture API is ambiguous.')
    api = functions[0]
    conditions = [node for node in ast.walk(api) if isinstance(node, ast.If)
                  and ast.unparse(node.test) == 'response.status >= 400']
    if len(conditions) != 1:
        raise ValueError('Native error branch is ambiguous.')
    branch = conditions[0]
    if (not isinstance(branch.body[0], ast.ImportFrom)
            or branch.body[0].module != 'native_http_diagnostics'
            or not isinstance(branch.body[1], ast.Expr)
            or ast.unparse(branch.body[1].value) != 'report_rejection(path, method, response.status, payload, ' + repr(role) + ')'):
        raise ValueError('Diagnostic insertion did not match its exact contract.')
    branch.body = branch.body[2:]
    if ast.dump(before, include_attributes=False) != ast.dump(after, include_attributes=False):
        raise ValueError('Diagnostic overlay changed existing fixture behavior.')
    if after_source.replace(insertion, '', 1) != source:
        raise ValueError('Diagnostic overlay was not an insertion only.')
    return after_source


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('repo', type=Path)
    parser.add_argument('role', choices=('reference', 'candidate'))
    args = parser.parse_args()
    target = args.repo / 'scripts/tests/native_runtime_test.py'
    source = target.read_text()
    instrumented = instrument(source, args.role)
    target.write_text(instrumented)
    shutil.copyfile(Path(__file__).with_name('native_http_diagnostics.py'), target.with_name('native_http_diagnostics.py'))
    print(json.dumps(dict(role=args.role, observationOnly=True,
                          originalSha256=hashlib.sha256(source.encode()).hexdigest(),
                          observedSha256=hashlib.sha256(instrumented.encode()).hexdigest())))


if __name__ == '__main__':
    main()
