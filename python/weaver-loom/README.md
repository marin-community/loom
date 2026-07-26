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

## Google workload credentials

Cloud Run, GCE, and other metadata-enabled Google workloads can authenticate
without a stored Loom token. Pulumi binds the workload service account's exact
numeric subject and email to a strict automation profile; `WorkloadCredentials`
retrieves an audience-bound Google identity token and exchanges it for a
short-lived, profile-scoped Loom JWT.

```python
from weaver_loom import Client, WorkloadCredentials

base = "https://loom.example.com"
client = Client(
    base=base,
    credentials=WorkloadCredentials(base),
    capabilities=["launch"],
)
run = client.run(
    "ops",
    idempotency_key="dashboard-alert-1842",
    session={
        "repo": "marin-community/marin",
        "goal": "Investigate alert 1842 and summarize the result",
    },
    source="ops",
)
```

Credentials are cached only in memory, refreshed early with jitter, and
invalidated for one retry after a 401. The helper never logs either upstream or
Loom tokens. The caller still needs the `launch` capability in its client
capability set if it constructs `Client` with an explicit restricted set.

## Tests

```sh
cd python/weaver-loom
uv run pytest
```
