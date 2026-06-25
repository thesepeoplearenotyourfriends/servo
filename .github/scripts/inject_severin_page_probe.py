#!/usr/bin/env python3
"""Inject the disposable page/world survey used by the probe-wheel workflow."""
from pathlib import Path

SOURCE = Path("ports/severin-python/src/lib.rs")
PROBE = Path("ports/severin-python/src/bridge_probe.rs")


def repl(text: str, old: str, new: str, name: str) -> str:
    found = text.count(old)
    if found != 1:
        raise SystemExit(f"SEVERIN_PROBE: expected one {name} anchor, found {found}")
    return text.replace(old, new, 1)


def main() -> None:
    text = SOURCE.read_text(encoding="utf-8")
    text = repl(text, "mod bridge_trace;\n", "mod bridge_trace;\nmod bridge_probe;\n", "module")
    text = repl(
        text,
        "    trace_bridge: bool,\n    next_evaluation_id: u64,\n",
        "    trace_bridge: bool,\n    probe_bridge: bool,\n    probe_snapshot: Option<String>,\n    next_evaluation_id: u64,\n",
        "app fields",
    )
    text = repl(
        text,
        '''const DRAIN_SCRIPT: &str = r#"
(() => {
  if (typeof globalThis.__severinDrain !== "function") {
    return JSON.stringify({ documentId: null, frames: [] });
  }
  return JSON.stringify(globalThis.__severinDrain(__SEVERIN_DRAIN_LIMIT__));
})()
"#;
''',
        '''const DRAIN_SCRIPT: &str = r#"
(() => {
  const err = x => String(x && x.message ? x.message : x || "Error");
  const safe = (name, fn) => { try { return fn(); } catch (x) { return { error: name + ":" + err(x) }; } };
  const doc = typeof globalThis.document === "object" ? globalThis.document : null;
  const win = typeof globalThis.window === "object" ? globalThis.window : null;
  const root = doc && doc.documentElement ? doc.documentElement : null;
  const body = doc && doc.body ? doc.body : null;
  const attr = name => root ? root.getAttribute(name) : null;
  const mark = target => !target || typeof target !== "object" ? { target: "absent" } : safe("bridge", () => ({
    target: "present",
    severin: typeof target.severin,
    send: target.severin ? typeof target.severin.send : "-",
    drain: typeof target.__severinDrain,
    resolve: typeof target.__severinResolve,
    ownSeverin: Object.prototype.hasOwnProperty.call(target, "severin"),
    ownDrain: Object.prototype.hasOwnProperty.call(target, "__severinDrain"),
    ownResolve: Object.prototype.hasOwnProperty.call(target, "__severinResolve"),
  }));
  let transport = { documentId: null, frames: [], rejectionDelta: null, shimPresent: false, drainError: null };
  if (typeof globalThis.__severinDrain === "function") {
    transport.shimPresent = true;
    try {
      const drained = globalThis.__severinDrain(__SEVERIN_DRAIN_LIMIT__);
      transport.documentId = typeof drained.documentId === "string" ? drained.documentId : null;
      transport.frames = Array.isArray(drained.frames) ? drained.frames : [];
      transport.rejectionDelta = drained.rejectionDelta || null;
    } catch (x) { transport.drainError = err(x); }
  }
  const probe = {
    schema: "severin-page-world-sweep-v1",
    dom: {
      documentPresent: Boolean(doc),
      url: safe("url", () => doc ? doc.URL : null),
      baseURI: safe("base", () => doc ? doc.baseURI : null),
      locationHref: safe("location", () => globalThis.location ? globalThis.location.href : null),
      readyState: safe("ready", () => doc ? doc.readyState : null),
      title: safe("title", () => doc ? doc.title : null),
      fixtureStatic: attr("data-severin-fixture"),
      staticMarker: attr("data-severin-static"),
      pageInline: attr("data-severin-page-inline"),
      pageLastPhase: attr("data-severin-page-last-phase"),
      pagePhaseCount: attr("data-severin-page-phase-count"),
      pageTimer: attr("data-severin-page-timer"),
      pageReport: attr("data-severin-page-report"),
      bodyPresent: Boolean(body),
      bodyStatic: body ? body.getAttribute("data-severin-static") : null,
      bodyLive: body ? body.getAttribute("data-severin-page-body-live") : null,
      scriptCount: safe("scripts", () => doc ? doc.scripts.length : null),
      scripts: safe("script-list", () => doc ? Array.from(doc.scripts, s => ({ id: s.id || "", src: s.getAttribute("src") || "", type: s.getAttribute("type") || "classic", inlineBytes: s.src ? 0 : s.textContent.length })) : []),
    },
    worlds: {
      identities: {
        globalEqualsWindow: globalThis === win,
        windowEqualsDefaultView: safe("window-default", () => win === (doc ? doc.defaultView : null)),
        globalEqualsDefaultView: safe("global-default", () => globalThis === (doc ? doc.defaultView : null)),
      },
      bridge: {
        globalThis: mark(globalThis),
        window: mark(win),
        defaultView: mark(safe("default-view", () => doc ? doc.defaultView : null)),
        parent: mark(safe("parent", () => globalThis.parent)),
        top: mark(safe("top", () => globalThis.top)),
      },
      primitives: {
        Promise: typeof globalThis.Promise,
        setTimeout: typeof globalThis.setTimeout,
        requestAnimationFrame: typeof globalThis.requestAnimationFrame,
        queueMicrotask: typeof globalThis.queueMicrotask,
        localStorage: safe("local-storage", () => typeof globalThis.localStorage),
        sessionStorage: safe("session-storage", () => typeof globalThis.sessionStorage),
      },
    },
  };
  return JSON.stringify({ documentId: transport.documentId, frames: transport.frames, rejectionDelta: transport.rejectionDelta, transport, probe });
})()
"#;
''',
        "drain script",
    )
    text = repl(
        text,
        '''        bridge_trace::emit(
            trace_bridge,
            format_args!("app:new width={width} height={height}"),
        );

        let mut event_loop = EventLoop::with_user_event()
''',
        '''        bridge_trace::emit(
            trace_bridge,
            format_args!("app:new width={width} height={height}"),
        );
        let probe_bridge = bridge_probe::enabled();
        bridge_probe::emit(format_args!("app:new page/world sweep enabled"));

        let mut event_loop = EventLoop::with_user_event()
''',
        "probe init",
    )
    text = repl(
        text,
        '''            pending_evaluations: Vec::new(),
            trace_bridge,
            next_evaluation_id: 0,
''',
        '''            pending_evaluations: Vec::new(),
            trace_bridge,
            probe_bridge,
            probe_snapshot: None,
            next_evaluation_id: 0,
''',
        "probe init fields",
    )
    text = repl(
        text,
        '''        self.pending_evaluations.clear();
        self.bridge_transport.clear_for_navigation();
''',
        '''        self.pending_evaluations.clear();
        self.bridge_transport.clear_for_navigation();
        self.probe_snapshot = None;
''',
        "navigation reset",
    )
    text = repl(
        text,
        '''        let drained: serde_json::Value = serde_json::from_str(&serialized)
            .map_err(|error| format!("invalid Severin bridge drain result: {error}"))?;
        let document_id = drained
''',
        '''        let drained: serde_json::Value = serde_json::from_str(&serialized)
            .map_err(|error| format!("invalid Severin bridge drain result: {error}"))?;
        if self.probe_bridge {
            let value = serde_json::json!({
                "transport": drained.get("transport").cloned().unwrap_or(serde_json::Value::Null),
                "probe": drained.get("probe").cloned().unwrap_or(serde_json::Value::Null),
            });
            let snapshot = serde_json::to_string(&value)
                .map_err(|error| format!("failed to serialize Severin probe snapshot: {error}"))?;
            if self.probe_snapshot.as_deref() != Some(snapshot.as_str()) {
                bridge_probe::emit(format_args!("world snapshot bytes={}", snapshot.len()));
                self.probe_snapshot = Some(snapshot);
            }
        }
        let document_id = drained
''',
        "snapshot collection",
    )
    text = repl(
        text,
        'unsafe extern "C" fn app_write(self_: *mut PyAppObject, args: *mut PyObject) -> *mut PyObject {\n',
        '''unsafe extern "C" fn app_probe_snapshot(
    self_: *mut PyAppObject,
    _args: *mut PyObject,
) -> *mut PyObject {
    let Ok(app) = (unsafe { get_app(self_) }) else {
        return ptr::null_mut();
    };
    let Some(snapshot) = app.probe_snapshot.as_deref() else {
        unsafe {
            Py_IncRef(py_none());
            return py_none();
        }
    };
    unsafe { PyUnicode_FromStringAndSize(snapshot.as_ptr().cast(), snapshot.len() as isize) }
}

unsafe extern "C" fn app_write(self_: *mut PyAppObject, args: *mut PyObject) -> *mut PyObject {
''',
        "Python snapshot method",
    )
    text = repl(text, "static mut APP_METHODS: [PyMethodDef; 8] = [\n", "static mut APP_METHODS: [PyMethodDef; 9] = [\n", "method count")
    text = repl(
        text,
        '''    PyMethodDef {
        ml_name: ptr::null(),
        ml_meth: ptr::null_mut(),
        ml_flags: 0,
        ml_doc: ptr::null(),
    },
''',
        '''    PyMethodDef {
        ml_name: c"probe_snapshot".as_ptr(),
        ml_meth: app_probe_snapshot as *mut c_void,
        ml_flags: METH_NOARGS,
        ml_doc: c"Read the current temporary page/world survey snapshot.".as_ptr(),
    },
    PyMethodDef {
        ml_name: ptr::null(),
        ml_meth: ptr::null_mut(),
        ml_flags: 0,
        ml_doc: ptr::null(),
    },
''',
        "method entry",
    )
    SOURCE.write_text(text, encoding="utf-8")
    PROBE.write_text(
        '''//! `SEVERIN_PROBE=1` diagnostics for the temporary page/world survey.
pub(crate) const ENV: &str = "SEVERIN_PROBE";
pub(crate) fn enabled() -> bool {
    match std::env::var(ENV) {
        Ok(value) => !matches!(value.as_str(), "" | "0" | "false" | "FALSE" | "off" | "OFF"),
        Err(_) => false,
    }
}
pub(crate) fn emit(message: std::fmt::Arguments<'_>) {
    if enabled() { eprintln!("SEVERIN_PROBE: {message}"); }
}
''',
        encoding="utf-8",
    )
    print("SEVERIN_PROBE: native page/world sweep injection complete")


if __name__ == "__main__":
    main()
