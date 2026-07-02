# weaver-loom

The Python layer over the loom REST API: fleet reads, capability-gated actions
(the intervention ladder), and the watch round context. Stdlib-only —
everything loom can do is exposed over HTTP, and this package is purely a
convenience layer on top.

Watch programs don't need to install it: the loom engine vendors the
module onto `PYTHONPATH` for every script it runs. For standalone iteration:

```sh
uv pip install -e python/weaver-loom
```

```python
from weaver_loom import Round

rnd = Round()  # reads $WEAVER_API + $WEAVER_WATCH
for session in rnd.sessions():
    ...
rnd.finish(f"surveyed {rnd.surveyed}, {len(rnd.actions)} findings")
```

The builtin programs under `crates/loom/watches/` are working examples;
the program contract is documented in `docs/ARCHITECTURE.md` (Watches).
