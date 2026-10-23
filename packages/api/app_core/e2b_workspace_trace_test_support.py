"""Portable deterministic corpus and commands for opt-in Core/E2B traces.

All workspace changes go through generated Core Bash tool requests. Logical
corpus sizes describe test data, not measured guest or provider physical IO.
"""
import inspect
import shlex


FIXED_SEED = b'centaeris-fixed-workspace-v1'
LARGE_BYTES = 8 * 1024 * 1024
FILE_BYTES = 4096
MANY_COUNT = 512
MANY_CHANGED = (3, 127, 255, 511)
CHANGE_OFFSET = 3 * 1024 * 1024
CASES = frozenset({'unchanged', 'reverted', 'smallChange', 'manyFiles'})


def fixed_trace_case(value):
    if not isinstance(value, str) or value not in CASES:
        raise ValueError('Explicit supported E2B fixed workspace trace case required')
    return value


def _corpus_bytes(label, size):
    import hashlib
    return hashlib.shake_256(FIXED_SEED + label.encode('ascii')).digest(size)


def _contents(case, final):
    if case == 'manyFiles':
        result = {f'files/file-{index:04d}.bin': _corpus_bytes(f'file-{index:04d}', FILE_BYTES)
                  for index in range(MANY_COUNT)}
        if final:
            for index in MANY_CHANGED:
                path = f'files/file-{index:04d}.bin'
                result[path] = bytes(value ^ 0x5A for value in result[path])
        return result
    content = _corpus_bytes('corpus', LARGE_BYTES)
    if final and case == 'smallChange':
        content = (content[:CHANGE_OFFSET]
            + bytes(value ^ 0x5A for value in content[CHANGE_OFFSET:CHANGE_OFFSET + FILE_BYTES])
            + content[CHANGE_OFFSET + FILE_BYTES:])
    return {'corpus.bin': content}


def _manifest(contents):
    import hashlib
    return {'schema': 'workspace.snapshot.v1', 'files': [
        {'path': path, 'sizeBytes': len(body), 'sha256': 'sha256:' + hashlib.sha256(body).hexdigest(),
         'executable': False} for path, body in sorted(contents.items())]}


def _actual_manifest(root):
    import hashlib
    files = []
    for path in sorted(root.rglob('*')):
        assert not path.is_symlink(), 'trace source cannot contain links'
        if path.is_dir():
            continue
        assert path.is_file(), 'trace source must be regular'
        body = path.read_bytes()
        files.append({'path': path.relative_to(root).as_posix(), 'sizeBytes': len(body),
            'sha256': 'sha256:' + hashlib.sha256(body).hexdigest(),
            'executable': bool(path.stat().st_mode & 0o111)})
    return {'schema': 'workspace.snapshot.v1', 'files': files}


def _run_trace(case, stage):
    import hashlib
    import json
    from pathlib import Path
    root = Path.cwd()
    original = _contents(case, False)
    if stage == 'prepare':
        assert not _actual_manifest(root)['files'], 'trace must start in an empty workspace'
        for relative, body in original.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(body)
            path.chmod(0o600)
    elif stage == 'mutate':
        assert _actual_manifest(root) == _manifest(original), 'restored trace does not match A'
        if case == 'reverted':
            path = root / 'corpus.bin'
            before = original['corpus.bin'][CHANGE_OFFSET:CHANGE_OFFSET + FILE_BYTES]
            with path.open('r+b') as stream:
                stream.seek(CHANGE_OFFSET)
                stream.write(bytes(value ^ 0x5A for value in before))
            assert path.read_bytes() != original['corpus.bin'], 'revert trace must first modify content'
            with path.open('r+b') as stream:
                stream.seek(CHANGE_OFFSET)
                stream.write(before)
        elif case == 'smallChange':
            with (root / 'corpus.bin').open('r+b') as stream:
                stream.seek(CHANGE_OFFSET)
                stream.write(_contents(case, True)['corpus.bin'][CHANGE_OFFSET:CHANGE_OFFSET + FILE_BYTES])
        elif case == 'manyFiles':
            final = _contents(case, True)
            for index in MANY_CHANGED:
                relative = f'files/file-{index:04d}.bin'
                (root / relative).write_bytes(final[relative])
    final = stage in ('mutate', 'verifyFinal')
    expected = _manifest(_contents(case, final))
    assert _actual_manifest(root) == expected, 'final trace manifest does not match'
    encoded = json.dumps(expected, ensure_ascii=False, separators=(',', ':')).encode('utf-8')
    print(json.dumps({'case': case, 'fileCount': len(expected['files']),
        'logicalBytes': sum(item['sizeBytes'] for item in expected['files']),
        'manifestSha256': 'sha256:' + hashlib.sha256(encoded).hexdigest()}, separators=(',', ':')))


def expected_manifest(case, *, final):
    fixed_trace_case(case)
    if type(final) is not bool:
        raise ValueError('Trace final state must be explicit')
    return _manifest(_contents(case, final))


def changed_bytes(case):
    fixed_trace_case(case)
    return FILE_BYTES * (len(MANY_CHANGED) if case == 'manyFiles' else int(case == 'smallChange'))


def trace_steps(case, run_index):
    fixed_trace_case(case)
    if type(run_index) is not int or run_index not in (0, 1):
        raise ValueError('Fixed trace has exactly two sequential Runs')
    constants = '\n'.join(f'{name} = {globals()[name]!r}' for name in
        ('FIXED_SEED', 'LARGE_BYTES', 'FILE_BYTES', 'MANY_COUNT', 'MANY_CHANGED', 'CHANGE_OFFSET'))
    functions = '\n\n'.join(inspect.getsource(function) for function in
        (_corpus_bytes, _contents, _manifest, _actual_manifest, _run_trace))
    source = constants + '\n\n' + functions
    stages = ('prepare', 'verifyInitial') if run_index == 0 else ('mutate', 'verifyFinal')
    return [('bash', {'command': 'python3 -c ' + shlex.quote(source + f'\n_run_trace({case!r}, {stage!r})')})
            for stage in stages]


def trace_workload_report(case):
    import hashlib
    import json
    before = expected_manifest(case, final=False)
    after = expected_manifest(case, final=True)
    def digest(manifest):
        encoded = json.dumps(manifest, ensure_ascii=False, separators=(',', ':')).encode('utf-8')
        return 'sha256:' + hashlib.sha256(encoded).hexdigest()
    return {'case': fixed_trace_case(case), 'seed': FIXED_SEED.decode('ascii'),
        'initialManifestSha256': digest(before), 'finalManifestSha256': digest(after),
        'fileCount': len(before['files']), 'logicalBytes': sum(file['sizeBytes'] for file in before['files']),
        'logicalChangedBytes': changed_bytes(case)}
