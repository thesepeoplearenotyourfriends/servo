#!/usr/bin/env python3
"""Inject a one-shot page-global bridge probe into an Actions worktree.

This script intentionally edits only the ephemeral worktree restored by the
probe workflow. It never writes a commit and is guarded by exact anchors so a
source drift becomes a clear workflow failure instead of a silent bad build.
"""

from __future__ import annotations

from pathlib import Path


SOURCE = Path("ports/severin-python/src/lib.rs")
PROBE_MODULE = Path("ports/severin-python/src/bridge_probe.rs")


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"SEVERIN_PROBE: expected one {label} anchor, found {count}")
    return text.replace(old, new, 1)


def main() -> None:
    text = SOURCE.read_text(encoding="utf-8")

    text = replace_once(
        text,
        "mod bridge_trace;\n",
        "mod bridge_trace;\nmod bridge_probe;\n",
        "module declaration",
    )
    text = replace_once(
        text,
        "    trace_bridge: bool,\n    next_evaluation_id: u64,\n",
        "    trace_bridge: bool,\n    probe_bridge: bool,\n    next_evaluation_id: u64,\n",
        "EmbeddedServoApp field",
    )
    text = replace_once(
        text,
        "    active_document_id: Option<String>,\n",
        "    active_document_id: Option<String>,\n    last_page_probe: Option<Option<String>>,\n",
        "BridgeTransport field",
    )
    text = replace_once(
        text,
        """    fn clear_for_navigation(&mut self) {
        self.inbound.clear();
        self.pending.clear();
        self.active_document_id = None;
    }
""",
        """    fn clear_for_navigation(&mut self) {
        self.inbound.clear();
        self.pending.clear();
        self.active_document_id = None;
        self.last_page_probe = None;
    }
""",
        "navigation clear",
    )
    text = replace_once(
        text,
        """    fn clear_for_close(&mut self) {
        self.clear_for_navigation();
    }
""",
        """    fn clear_for_close(&mut self) {
        self.clear_for_navigation();
    }

    fn observe_page_probe(&mut self, page_probe: Option<String>) -> bool {
        if self.last_page_probe == Some(page_probe.clone()) {
            return false;
        }
        self.last_page_probe = Some(page_probe);
        true
    }
""",
        "probe observer",
    )
    text = replace_once(
        text,
        """const DRAIN_SCRIPT: &str = r#"
(() => {
  if (typeof globalThis.__severinDrain !== "function") {
    return JSON.stringify({ documentId: null, frames: [] });
  }
  return JSON.stringify(globalThis.__severinDrain());
})()
"#;
""",
        """const DRAIN_SCRIPT: &str = r#"
(() => {
  const root = globalThis.document ? globalThis.document.documentElement : null;
  const pageProbe = root ? root.getAttribute("data-severin-probe") : null;
  if (typeof globalThis.__severinDrain !== "function") {
    return JSON.stringify({ documentId: null, frames: [], pageProbe });
  }
  const drained = globalThis.__severinDrain();
  return JSON.stringify({
    documentId: drained.documentId,
    frames: drained.frames,
    pageProbe,
  });
})()
"#;
""",
        "drain script",
    )
    text = replace_once(
        text,
        """        bridge_trace::emit(
            trace_bridge,
            format_args!("app:new width={width} height={height}"),
        );

        let mut event_loop = EventLoop::with_user_event()
""",
        """        bridge_trace::emit(
            trace_bridge,
            format_args!("app:new width={width} height={height}"),
        );
        let probe_bridge = bridge_probe::enabled();
        bridge_probe::emit(format_args!("app:new page-global probe enabled"));

        let mut event_loop = EventLoop::with_user_event()
""",
        "probe initialization",
    )
    text = replace_once(
        text,
        """            pending_evaluations: Vec::new(),
            trace_bridge,
            next_evaluation_id: 0,
""",
        """            pending_evaluations: Vec::new(),
            trace_bridge,
            probe_bridge,
            next_evaluation_id: 0,
""",
        "probe field initialization",
    )
    text = replace_once(
        text,
        """        let drained: serde_json::Value = serde_json::from_str(&serialized)
            .map_err(|error| format!("invalid Severin bridge drain result: {error}"))?;
        let document_id = drained
""",
        """        let drained: serde_json::Value = serde_json::from_str(&serialized)
            .map_err(|error| format!("invalid Severin bridge drain result: {error}"))?;
        if self.probe_bridge {
            let page_probe = drained
                .get("pageProbe")
                .and_then(|value| value.as_str())
                .map(str::to_owned);
            if self.bridge_transport.observe_page_probe(page_probe.clone()) {
                bridge_probe::emit(format_args!(
                    "page-global={}",
                    page_probe.as_deref().unwrap_or("<absent>")
                ));
            }
        }
        let document_id = drained
""",
        "page-probe read",
    )

    SOURCE.write_text(text, encoding="utf-8")
    PROBE_MODULE.write_text(
        """//! `SEVERIN_PROBE=1` diagnostics for page-visible bridge state.

/// Opt-in environment variable for the bounded page-global probe.
pub(crate) const ENV: &str = "SEVERIN_PROBE";

pub(crate) fn enabled() -> bool {
    match std::env::var(ENV) {
        Ok(value) => !matches!(value.as_str(), "" | "0" | "false" | "FALSE" | "off" | "OFF"),
        Err(_) => false,
    }
}

pub(crate) fn emit(message: std::fmt::Arguments<'_>) {
    if enabled() {
        eprintln!("SEVERIN_PROBE: {message}");
    }
}
""",
        encoding="utf-8",
    )
    print("SEVERIN_PROBE: native source injection complete")


if __name__ == "__main__":
    main()
