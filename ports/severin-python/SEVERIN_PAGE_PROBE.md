# Page-global probe

This branch is a review/build aid only. It is based on this fork's `main` and adds exactly two executable artifacts:

- `severin-page-probe.patch` — adds an opt-in `SEVERIN_PROBE=1` diagnostic path to the existing CPython extension.
- `apply-severin-page-probe.sh` — applies that patch once, validates it first, and then runs the build command passed to it.

It does not open or target an upstream Servo pull request. Do not use GitHub's “Compare & pull request” button for this branch.

The patch does three things:

1. Reads the document element's `data-severin-probe` attribute from the native drain evaluator.
2. Returns that observation beside the ordinary drain result without changing bridge payloads.
3. Emits only changed values as `SEVERIN_PROBE: page-global=...` when the environment variable is enabled.

Build shape:

```bash
bash ports/severin-python/apply-severin-page-probe.sh \
  cargo build --release -p severin-python
```

Run shape after the usual artifact transfer:

```bash
SEVERIN_TRACE=1 SEVERIN_PROBE=1 \
  /test_html/run_severin_xvfb.sh /test_html/severin_probe_smoke.py
```
