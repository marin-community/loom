//! `weaver_py` — a Pythonic, synchronous wrapper over [`weaver_api`].
//!
//! This is the out-of-process seam from the overlooker design: a scripted
//! overlooker (or an agent iterating on one, or a human at a REPL) drives the
//! loom fleet through the same typed REST surface the `loom` CLI uses, never
//! touching tmux directly. The loom daemon stays the single owner of the live
//! runtime.
//!
//! Two design points worth stating:
//!
//! - **Synchronous API.** `weaver_api::Client` is async; rather than push
//!   `async`/`await` into Python (pyo3-asyncio), each method drives a private
//!   single-thread tokio runtime to completion with `block_on`. Python callers
//!   see plain blocking methods.
//! - **Capability enforcement lives below the glue.** Every mutating method
//!   calls [`weaver_api::require`] — the pure, workspace-tested gate — *before*
//!   it issues a request, so a `Client` built without `nudge` cannot nudge even
//!   if the server would allow it. Read methods need only the implicit
//!   `observe`.
//!
//! DTOs cross into Python as plain dicts via `serde_json` → `pythonize`, so a
//! script reads `s["id"]`, `s["branch"]["description"]`, and the branch's
//! `tags` (a list of `{key, value, note, set_by, set_at}`) without a bespoke
//! wrapper class per View. The well-known keys are `attention` (the agent's
//! self-report) and `triage` (an overlooker's assessment); absence is the calm
//! state — there is no `ok` tag.

use pyo3::exceptions::{PyNotImplementedError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pythonize::pythonize;
use serde::Serialize;

use weaver_api::capability::{require, CapabilityError};
use weaver_api::{Client as ApiClient, SendReq};

pyo3::create_exception!(
    weaver_py,
    WeaverError,
    PyRuntimeError,
    "An error from the loom server or transport (a failed request, a decode \
     error, an unreachable server)."
);

pyo3::create_exception!(
    weaver_py,
    CapabilityDenied,
    PyValueError,
    "A mutating call was attempted without the capability it requires. The \
     client's granted capability set is fixed at construction; this is the \
     security contract of the intervention ladder."
);

/// `$WEAVER_API` resolved to a base URL, mirroring `loom::endpoint`: a URL or a
/// bare `host:port` is accepted; an unset/empty value falls back to the loom
/// default `127.0.0.1:7878`. The `server.json` discovery the CLI also does is
/// loom-internal and intentionally not replicated here — set `$WEAVER_API` (or
/// pass `base=`) to target a non-default server.
const DEFAULT_ADDR: &str = "127.0.0.1:7878";

fn default_base() -> String {
    match std::env::var("WEAVER_API") {
        Ok(v) if !v.trim().is_empty() => {
            let s = v.trim();
            if s.starts_with("http://") || s.starts_with("https://") {
                s.trim_end_matches('/').to_string()
            } else {
                format!("http://{}", s.trim_end_matches('/'))
            }
        }
        _ => format!("http://{DEFAULT_ADDR}"),
    }
}

/// Map a `weaver_api` (anyhow) transport/server error to a Python `WeaverError`.
fn api_err(e: anyhow::Error) -> PyErr {
    WeaverError::new_err(e.to_string())
}

/// Map a denied capability to a Python `CapabilityDenied`.
fn cap_err(e: CapabilityError) -> PyErr {
    CapabilityDenied::new_err(e.to_string())
}

/// Serialize a serde value into a Python object (dict/list/scalar) via
/// pythonize, surfacing a failure as a `WeaverError` rather than a panic.
fn to_py<T: Serialize>(py: Python<'_>, value: &T) -> PyResult<Py<PyAny>> {
    pythonize(py, value)
        .map(|b| b.into())
        .map_err(|e| WeaverError::new_err(format!("encoding response for Python: {e}")))
}

/// A capability-gated, synchronous client for one loom server.
///
/// Construct it with the granted capability set (the intervention ladder);
/// `observe` is implicit, so read methods always work. Each mutating method
/// gates on its capability first and raises `CapabilityDenied` if it is absent.
#[pyclass]
struct Client {
    inner: ApiClient,
    rt: tokio::runtime::Runtime,
    granted: Vec<String>,
}

impl Client {
    /// Gate `needed` against the granted set, raising `CapabilityDenied`.
    fn gate(&self, needed: &str) -> PyResult<()> {
        require(&self.granted, needed).map_err(cap_err)
    }
}

#[pymethods]
impl Client {
    /// `Client(base=None, capabilities=None)`.
    ///
    /// `base` defaults to `$WEAVER_API` (or the loom default `127.0.0.1:7878`).
    /// `capabilities` is the granted set; `observe` is always implied, so an
    /// empty/omitted set is read-only.
    #[new]
    #[pyo3(signature = (base=None, capabilities=None))]
    fn new(base: Option<String>, capabilities: Option<Vec<String>>) -> PyResult<Self> {
        let base = base.filter(|b| !b.trim().is_empty()).unwrap_or_else(default_base);
        // A current-thread runtime: the binding makes one blocking request at a
        // time, so a multi-thread pool would only add idle worker threads.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| WeaverError::new_err(format!("building async runtime: {e}")))?;
        Ok(Self {
            inner: ApiClient::new(base),
            rt,
            granted: capabilities.unwrap_or_default(),
        })
    }

    /// The base URL this client targets.
    #[getter]
    fn base(&self) -> &str {
        self.inner.base()
    }

    /// The granted capability set (excluding the implicit `observe`).
    #[getter]
    fn capabilities(&self) -> Vec<String> {
        self.granted.clone()
    }

    /// Whether this client holds `cap` (`observe` is always true). Mirrors the
    /// engine's `ov.can(...)` so a program can branch on its own grants.
    fn can(&self, cap: &str) -> bool {
        require(&self.granted, cap).is_ok()
    }

    // -- Reads (observe) --------------------------------------------------

    /// Every active session, as a list of dicts (`GET /api/sessions`).
    fn sessions(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let views = py
            .detach(|| self.rt.block_on(self.inner.list_sessions()))
            .map_err(api_err)?;
        to_py(py, &views)
    }

    /// One session by key — id, branch id, branch name, or `repo:branch`
    /// (`GET /api/sessions/{key}`).
    fn session(&self, py: Python<'_>, key: &str) -> PyResult<Py<PyAny>> {
        let view = py
            .detach(|| self.rt.block_on(self.inner.get_session(key)))
            .map_err(api_err)?;
        to_py(py, &view)
    }

    /// The session's tmux pane as plain text, with `lines` of scrollback above
    /// the visible screen (`GET /api/sessions/{key}/preview`).
    #[pyo3(signature = (key, lines=0))]
    fn preview(&self, py: Python<'_>, key: &str, lines: usize) -> PyResult<String> {
        py.detach(|| self.rt.block_on(self.inner.preview(key, lines)))
            .map_err(api_err)
    }

    /// The worktree file tree + change map vs the diff base, as a dict
    /// (`GET /api/sessions/{key}/tree`).
    fn diff(&self, py: Python<'_>, key: &str) -> PyResult<Py<PyAny>> {
        let value = py
            .detach(|| self.rt.block_on(self.inner.diff(key)))
            .map_err(api_err)?;
        to_py(py, &value)
    }

    // -- Writes (capability-gated) ----------------------------------------

    /// Set (upsert) a tag on a session (needs `mark`). `tag_key` is the axis
    /// (`attention`, `triage`, or any free-form key); for a loud key `value` is
    /// `attention` | `blocked` — clear the tag (rather than setting `ok`) to
    /// return to calm. `by` defaults to `manual` server-side. Returns the
    /// updated session.
    #[pyo3(signature = (key, tag_key, value, note="", by=None))]
    fn set_tag(
        &self,
        py: Python<'_>,
        key: &str,
        tag_key: &str,
        value: &str,
        note: &str,
        by: Option<String>,
    ) -> PyResult<Py<PyAny>> {
        self.gate("mark")?;
        let view = py
            .detach(|| {
                self.rt
                    .block_on(self.inner.set_tag(key, tag_key, value, note, by.as_deref()))
            })
            .map_err(api_err)?;
        to_py(py, &view)
    }

    /// Clear a tag on a session (needs `mark`) — how a loud axis returns to
    /// calm. `by` attributes the clear on the audit event. Returns the
    /// updated session.
    #[pyo3(signature = (key, tag_key, by=None))]
    fn clear_tag(
        &self,
        py: Python<'_>,
        key: &str,
        tag_key: &str,
        by: Option<String>,
    ) -> PyResult<Py<PyAny>> {
        self.gate("mark")?;
        let view = py
            .detach(|| {
                self.rt
                    .block_on(self.inner.clear_tag(key, tag_key, by.as_deref()))
            })
            .map_err(api_err)?;
        to_py(py, &view)
    }

    /// Stamp the overlooker's triage mark on a session (needs `mark`) — a
    /// convenience over the `triage` tag. `level` is `attention` | `blocked` to
    /// raise it, or empty/`ok` to clear it; `by` defaults to `manual`
    /// server-side. Returns the updated session.
    #[pyo3(signature = (key, level, note="", by=None))]
    fn mark(
        &self,
        py: Python<'_>,
        key: &str,
        level: &str,
        note: &str,
        by: Option<String>,
    ) -> PyResult<Py<PyAny>> {
        self.gate("mark")?;
        let view = py
            .detach(|| {
                self.rt
                    .block_on(self.inner.mark(key, level, note, by.as_deref()))
            })
            .map_err(api_err)?;
        to_py(py, &view)
    }

    /// Type a message into a session's agent pane (needs `nudge`). `submit`
    /// presses Enter to start a turn; pass `False` to stage input unsubmitted.
    /// `by` attributes the recorded `nudge` audit event. Returns the raw
    /// `{sent, submitted}` reply.
    #[pyo3(signature = (key, text, submit=true, by=None))]
    fn nudge(
        &self,
        py: Python<'_>,
        key: &str,
        text: &str,
        submit: bool,
        by: Option<String>,
    ) -> PyResult<Py<PyAny>> {
        self.gate("nudge")?;
        let req = SendReq {
            text: text.to_string(),
            submit,
            by,
        };
        let value = py
            .detach(|| self.rt.block_on(self.inner.nudge(key, &req)))
            .map_err(api_err)?;
        to_py(py, &value)
    }

    /// Send a break (Escape) to interrupt the agent's current turn (needs
    /// `interrupt`). Returns the raw server reply.
    fn interrupt(&self, py: Python<'_>, key: &str) -> PyResult<Py<PyAny>> {
        self.gate("interrupt")?;
        let value = py
            .detach(|| self.rt.block_on(self.inner.interrupt(key)))
            .map_err(api_err)?;
        to_py(py, &value)
    }

    /// The persistent overlooker session, created-or-reused across rounds.
    ///
    /// Not yet available: the warm-session lifecycle is T12 of the overlooker
    /// plan and has no `weaver-api` backing today. It raises rather than faking
    /// a session so a program never silently no-ops.
    fn warm_session(&self) -> PyResult<Py<PyAny>> {
        Err(PyNotImplementedError::new_err(
            "warm_session() arrives with the warm-session lifecycle (overlooker plan T12); \
             it is not yet backed by weaver-api",
        ))
    }

    /// Spawn a fresh one-shot agent for a judgement call (needs `launch`).
    ///
    /// Not yet available: `run_agent` is an in-process loom helper (the
    /// env-stripped `claude -p` pattern), driven by the engine, not the REST
    /// client — there is no out-of-process endpoint for it today. The
    /// capability is checked first so a program without `launch` gets the
    /// capability error regardless.
    fn run_agent(&self, _prompt: &str) -> PyResult<Py<PyAny>> {
        self.gate("launch")?;
        Err(PyNotImplementedError::new_err(
            "run_agent() is an in-process loom helper not exposed over weaver-api; \
             it is driven by the engine (overlooker plan T5), not this client",
        ))
    }
}

#[pymodule]
fn weaver_py(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Client>()?;
    m.add("WeaverError", m.py().get_type::<WeaverError>())?;
    m.add("CapabilityDenied", m.py().get_type::<CapabilityDenied>())?;
    // The capability vocabulary, so a script can introspect the ladder.
    m.add("CAPABILITIES", weaver_api::capability::CAPABILITIES.to_vec())?;
    m.add(
        "__doc__",
        "Capability-gated Python bindings over the loom REST API.",
    )?;
    Ok(())
}
