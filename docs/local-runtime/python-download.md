# Severin Python launcher wheel download

The visible Python distribution path is the pure-Python `severin` launcher/controller package. It launches the single headed `severin` executable and communicates only through inherited anonymous pipe FDs when bridge mode is enabled.

```text
severin-<version>-py3-none-any.whl
```

It is not the old in-process CPython extension and does not embed Servo in Python.

## Offline install

1. Download the matching `.whl` from the repository's Releases page.
2. Install from the local file without dependency resolution:

   ```bash
   python3 -m pip install --user --no-deps ./severin-<version>-py3-none-any.whl
   ```

3. Verify import through Python's normal package importer:

   ```bash
   python3 -c 'import severin; print(severin.App)'
   ```

The package does not use PyPI, does not download dependencies, does not run Cargo on the user's machine, does not embed Servo, and does not bind or connect to a localhost service, HTTP endpoint, WebSocket, TCP/UDP socket, or named Unix socket. The experimental native extension has been moved out of the normal `import severin` identity and should be treated as separate/manual embedding work.
