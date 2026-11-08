# Observer only: retain every journey, assertion and timeout; repeat the parameter set.
import hashlib
import json
import subprocess
import sys
from pathlib import Path

root = Path(sys.argv[1])
patch = Path(sys.argv[2])
source = root / "apps/android/app/src/test/java/build/cairn/app/ui/UnifiedHeaderTest.kt"
original = source.read_bytes()
text = original.decode()
assert text.count('@Parameters(name = "cutout={0}dp")') == 1
assert text.count('fun cutouts() = listOf(arrayOf(0), arrayOf(48))') == 1
text = text.replace('@Parameters(name = "cutout={0}dp")', '@Parameters(name = "trial={index}, cutout={0}dp")')
text = text.replace('fun cutouts() = listOf(arrayOf(0), arrayOf(48))', 'fun cutouts() = (1..50).flatMap { listOf(arrayOf(0), arrayOf(48)) }')
source.write_text(text)
patch.write_bytes(subprocess.check_output(["git", "diff", "--", str(source)], cwd=root))
patch.with_suffix(".json").write_text(json.dumps({"trialsPerCutout": 50, "originalSha256": hashlib.sha256(original).hexdigest(), "observedSha256": hashlib.sha256(source.read_bytes()).hexdigest(), "patchSha256": hashlib.sha256(patch.read_bytes()).hexdigest()}, indent=2))
