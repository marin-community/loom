"""The security contract: capability gating, with no server required.

These run in CI (the `python-binding` job). They assert that a `Client` built
without a capability *cannot* perform the action it gates — the gate fires
before any request leaves the process, so no loom server is involved. This is
the one test that must always pass: it is the proof that the intervention
ladder is enforced at the binding.
"""

import weaver_py
import pytest


def test_module_exposes_the_ladder():
    assert weaver_py.CAPABILITIES == [
        "observe",
        "mark",
        "escalate",
        "nudge",
        "interrupt",
        "launch",
    ]


def test_default_base_is_the_loom_default():
    c = weaver_py.Client()
    assert c.base == "http://127.0.0.1:7878"
    # No capabilities granted -> read-only.
    assert c.capabilities == []
    assert c.can("observe")
    assert not c.can("mark")


def test_explicit_base_is_used_verbatim():
    c = weaver_py.Client(base="http://10.0.0.5:9000")
    assert c.base == "http://10.0.0.5:9000"


def test_can_reflects_the_granted_set():
    c = weaver_py.Client(capabilities=["mark", "nudge"])
    assert c.can("observe")  # implicit
    assert c.can("mark")
    assert c.can("nudge")
    assert not c.can("interrupt")
    assert not c.can("launch")


@pytest.mark.parametrize("method,args", [
    ("set_tag", ("sess", "triage", "attention")),
    ("clear_tag", ("sess", "triage")),
    ("mark", ("sess", "attention")),
    ("nudge", ("sess", "hello")),
    ("interrupt", ("sess",)),
])
def test_mutating_calls_raise_when_capability_absent(method, args):
    # Granted nothing but the implicit observe.
    c = weaver_py.Client(capabilities=[])
    with pytest.raises(weaver_py.CapabilityDenied):
        getattr(c, method)(*args)


def test_set_tag_requires_mark_capability():
    c = weaver_py.Client(capabilities=["nudge", "interrupt"])  # everything but mark
    with pytest.raises(weaver_py.CapabilityDenied):
        c.set_tag("sess", "triage", "blocked", "stuck on tests")


def test_clear_tag_requires_mark_capability():
    c = weaver_py.Client(capabilities=["nudge", "interrupt"])  # everything but mark
    with pytest.raises(weaver_py.CapabilityDenied):
        c.clear_tag("sess", "triage")


def test_mark_requires_mark_capability():
    c = weaver_py.Client(capabilities=["nudge", "interrupt"])  # everything but mark
    with pytest.raises(weaver_py.CapabilityDenied):
        c.mark("sess", "blocked", "stuck on tests")


def test_nudge_requires_nudge_capability():
    c = weaver_py.Client(capabilities=["mark", "interrupt"])  # everything but nudge
    with pytest.raises(weaver_py.CapabilityDenied):
        c.nudge("sess", "try the tests again")


def test_interrupt_requires_interrupt_capability():
    c = weaver_py.Client(capabilities=["mark", "nudge"])  # everything but interrupt
    with pytest.raises(weaver_py.CapabilityDenied):
        c.interrupt("sess")


def test_capability_denied_fires_before_any_network_call():
    # The gate must precede the request: point at an unroutable address and a
    # missing-capability call still raises CapabilityDenied (not a connection
    # error), proving nothing left the process.
    c = weaver_py.Client(base="http://127.0.0.1:1", capabilities=[])
    with pytest.raises(weaver_py.CapabilityDenied):
        c.mark("sess", "attention")


def test_run_agent_requires_launch_before_reporting_unimplemented():
    # Without launch, the gate fires first.
    c = weaver_py.Client(capabilities=[])
    with pytest.raises(weaver_py.CapabilityDenied):
        c.run_agent("is this stuck?")
    # With launch, it surfaces as not-yet-implemented (engine-only helper).
    c2 = weaver_py.Client(capabilities=["launch"])
    with pytest.raises(NotImplementedError):
        c2.run_agent("is this stuck?")


def test_warm_session_is_not_yet_implemented():
    c = weaver_py.Client(capabilities=["observe"])
    with pytest.raises(NotImplementedError):
        c.warm_session()
