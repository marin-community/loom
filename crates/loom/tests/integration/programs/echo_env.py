"""Test fixture: expose whether a watch subprocess inherited ``GH_TOKEN``."""

import os

from weaver_loom import Round

rnd = Round()
rnd.finish("token[" + (os.environ.get("GH_TOKEN") or "") + "]")
