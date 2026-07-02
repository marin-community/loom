"""Test fixture program: a reactive watch that surveys only the session the
triggering event named (via `triggered_sessions`), recording one `survey`
action per session. Declares a `pr.merged` subscription so a test can drive it
through the engine's register-mode reconcile and event dispatch.

Owned by the engine-mechanics integration tests (`tests/integration/
watches.rs`); it mutates nothing.
"""

from weaver_loom import Round

TRIGGERS = {"on": ["pr.merged"]}


def main(rnd):
    for session in rnd.triggered_sessions():
        rnd.did("survey", session=session["id"])
    rnd.finish(f"surveyed {rnd.surveyed}")


if __name__ == "__main__":
    Round.main(main, TRIGGERS)
