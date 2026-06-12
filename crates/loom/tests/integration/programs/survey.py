"""Test fixture program: record one `survey` action per surveyed session.

Owned by the engine-mechanics integration tests (`tests/integration/
overlookers.rs`): it mutates nothing, so a test can assert "a round ran over
exactly these sessions" from the run row alone — no dependency on any
builtin program's behavior.
"""

from weaver_loom import Round

rnd = Round()
for session in rnd.sessions():
    rnd.did("survey", session=session["id"])
rnd.finish(f"surveyed {rnd.surveyed}")
