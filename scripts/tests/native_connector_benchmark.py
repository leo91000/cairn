"""Synthetic loopback manager data for timing the production native connector."""
import base64
import io
import json
from pathlib import Path
import random
import stat


SKILLS = Path('home/.agents/skills')
SKILL_KEY = 'x-cairn-seed-skills-key'
SKILL_HIT = 'x-cairn-seed-skills-hit'


def seed_frames(root, omit_skills=False):
    stream = io.BytesIO()

    def frame(value):
        stream.write(json.dumps(value, separators=(',', ':')).encode() + b'\n')

    for entry in sorted(root.rglob('*')):
        relative = entry.relative_to(root)
        if omit_skills and relative.is_relative_to(SKILLS):
            continue
        if relative.as_posix().endswith('.codex/auth.json'):
            continue
        metadata = entry.lstat()
        mode = stat.S_IMODE(metadata.st_mode) & 0o777
        if stat.S_ISDIR(metadata.st_mode):
            frame(dict(kind='directory', path=relative.as_posix(), mode=mode))
        elif stat.S_ISREG(metadata.st_mode):
            frame(dict(kind='file', path=relative.as_posix(), mode=mode, size=metadata.st_size))
            with entry.open('rb') as source:
                while chunk := source.read(64 * 1024):
                    frame(dict(data=base64.b64encode(chunk).decode('ascii')))
    frame(dict(complete=True))
    return stream.getvalue()


def skill_bundle(stage, sample):
    root = stage / SKILLS
    random_bytes = random.Random(sample)
    for index in range(64):
        (root / 'frozen' / f'asset-{index:02}.bin').write_bytes(random_bytes.randbytes(32 * 1024))
