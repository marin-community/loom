"""Make the package importable straight from the source tree, so the suite
runs with a bare `pytest` (CI and local) — no install step, matching how the
loom engine itself vendors the module onto PYTHONPATH."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src"))
