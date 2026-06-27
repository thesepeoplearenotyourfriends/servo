"""Pure-Python Severin launcher/controller for the headed child runtime.

This module launches the single headed Severin executable and talks to it only
through two inherited anonymous pipe file descriptors. It does not embed Servo
and does not own the GUI/rendering event loop.
"""

from __future__ import annotations

from dataclasses import dataclass
import json
import logging
import os
from pathlib import Path
import queue
import struct
import subprocess
import threading
from typing import Callable, Optional

_FRAME_HEADER = struct.Struct(">IQ")
_DEFAULT_PACKAGE_ID = "com.example.app"
_DEFAULT_MAX_FRAME_BYTES = 1024 * 1024
_MAX_BRIDGE_POLL_MS = 60_000
_MAX_STARTUP_RETRIES = 16

BridgeCallback = Callable[[int, str], Optional[str]]


class SeverinTransportError(RuntimeError):
    """Raised when the private inherited-FD transport is closed or malformed."""


@dataclass(frozen=True)
class BridgeTiming:
    """Physical polling and startup timing for one private JS↔Python bridge.

    These quantities affect only transport scheduling. They do not define
    application operation names, JSON schemas, permissions, or reply meanings.
    """

    idle_poll_ms: int = 100
    busy_poll_ms: int = 10
    startup_retry_ms: tuple[int, ...] = (50, 100, 250, 500, 1_000, 2_000)

    def __post_init__(self) -> None:
        self._validate_delay("idle_poll_ms", self.idle_poll_ms)
        self._validate_delay("busy_poll_ms", self.busy_poll_ms)
        try:
            retry_delays = tuple(self.startup_retry_ms)
        except TypeError as exc:
            raise TypeError("startup_retry_ms must be an iterable of integer milliseconds") from exc
        if not retry_delays or len(retry_delays) > _MAX_STARTUP_RETRIES:
            raise ValueError(
                f"startup_retry_ms must contain between 1 and {_MAX_STARTUP_RETRIES} delays"
            )
        for delay in retry_delays:
            self._validate_delay("startup_retry_ms", delay)
        object.__setattr__(self, "startup_retry_ms", retry_delays)

    @staticmethod
    def _validate_delay(name: str, value: int) -> None:
        if type(value) is not int:
            raise TypeError(f"{name} values must be integers")
        if value <= 0 or value > _MAX_BRIDGE_POLL_MS:
            raise ValueError(
                f"{name} values must be between 1 and {_MAX_BRIDGE_POLL_MS} milliseconds"
            )

    def _child_args(self) -> list[str]:
        retry_schedule = ",".join(str(delay) for delay in self.startup_retry_ms)
        return [
            f"--bridge-idle-poll-ms={self.idle_poll_ms}",
            f"--bridge-busy-poll-ms={self.busy_poll_ms}",
            f"--bridge-startup-retry-ms={retry_schedule}",
        ]


class App:
    def __init__(
        self,
        *,
        width: int = 800,
        height: int = 600,
        bridge: BridgeCallback | None = None,
        executable: str | os.PathLike[str] | None = None,
        package_id: str = _DEFAULT_PACKAGE_ID,
        max_frame_bytes: int = _DEFAULT_MAX_FRAME_BYTES,
        bridge_timing: BridgeTiming | None = None,
    ) -> None:
        if width <= 0 or height <= 0:
            raise ValueError("width and height must be positive")
        if max_frame_bytes <= 0 or max_frame_bytes > 0xFFFF_FFFF:
            raise ValueError("max_frame_bytes must fit in u32 and be positive")
        if bridge_timing is not None and not isinstance(bridge_timing, BridgeTiming):
            raise TypeError("bridge_timing must be a BridgeTiming instance or None")

        self.width = int(width)
        self.height = int(height)
        self.bridge = bridge
        self.executable = str(executable or os.environ.get("SEVERIN_EXECUTABLE", "severin"))
        self.package_id = package_id
        self.max_frame_bytes = int(max_frame_bytes)
        self.bridge_timing = bridge_timing or BridgeTiming()

        self._package_root: Path | None = None
        self._entry: str | None = None
        self._process: subprocess.Popen[bytes] | None = None
        self._reply_fd: int | None = None
        self._request_fd: int | None = None
        self._write_queue: queue.Queue[tuple[int, str] | None] = queue.Queue(maxsize=128)
        self._writer: threading.Thread | None = None
        self._closed = threading.Event()
        self._state_lock = threading.Lock()
        self._run_error: BaseException | None = None

    def load_path(self, path: str | os.PathLike[str]) -> None:
        entry = Path(path).expanduser().resolve(strict=True)
        if not entry.is_file():
            raise ValueError("load_path() requires an existing file")
        root = entry.parent
        self._package_root = root
        self._entry = entry.relative_to(root).as_posix()

    def run(self) -> None:
        if self._package_root is None or self._entry is None:
            raise RuntimeError("load_path() must be called before run()")
        with self._state_lock:
            if self._process is not None:
                raise RuntimeError("App is already running")
            request_read, request_write = os.pipe()
            reply_read, reply_write = os.pipe()
            try:
                os.set_inheritable(request_write, True)
                os.set_inheritable(reply_read, True)
                argv = [
                    self.executable,
                    f"--severin-package-id={self.package_id}",
                    f"--severin-package-root={self._package_root}",
                    f"--severin-entry={self._entry}",
                    f"--bridge-request-fd={request_write}",
                    f"--bridge-reply-fd={reply_read}",
                    *self.bridge_timing._child_args(),
                    f"--window-size={self.width}x{self.height}",
                    "--no-egui",
                ]
                self._process = subprocess.Popen(
                    argv,
                    pass_fds=(request_write, reply_read),
                    close_fds=True,
                )
            finally:
                os.close(request_write)
                os.close(reply_read)
            self._request_fd = request_read
            self._reply_fd = reply_write
            self._writer = threading.Thread(
                target=self._writer_main,
                name="severin-reply-writer",
                daemon=True,
            )
            self._writer.start()
        try:
            self._reader_main(request_read)
        finally:
            self.close()
            process = self._process
            if process is not None:
                process.wait()
            if self._run_error is not None:
                error = self._run_error
                self._run_error = None
                raise error

    def write(self, receipt: int, json_text: str) -> None:
        if not isinstance(receipt, int) or receipt < 0:
            raise ValueError("receipt must be a non-negative integer")
        if not isinstance(json_text, str):
            raise TypeError("json_text must be a serialized JSON string")
        self._validate_json(json_text)
        if self._closed.is_set():
            raise SeverinTransportError("Severin child transport is closed")
        try:
            self._write_queue.put_nowait((receipt, json_text))
        except queue.Full as exc:
            raise SeverinTransportError("Severin reply queue is full") from exc

    def close(self) -> None:
        if self._closed.is_set():
            return
        self._closed.set()
        self._write_queue.put(None)
        for attr in ("_request_fd", "_reply_fd"):
            fd = getattr(self, attr)
            if fd is not None:
                try:
                    os.close(fd)
                except OSError:
                    pass
                setattr(self, attr, None)
        process = self._process
        if process is not None and process.poll() is None:
            process.terminate()

    def _reader_main(self, fd: int) -> None:
        buffer = bytearray()
        while not self._closed.is_set():
            try:
                chunk = os.read(fd, 8192)
            except OSError as exc:
                if self._closed.is_set():
                    return
                raise SeverinTransportError(f"request pipe read failed: {exc}") from exc
            if not chunk:
                return
            buffer.extend(chunk)
            while len(buffer) >= _FRAME_HEADER.size:
                length, receipt = _FRAME_HEADER.unpack(buffer[:_FRAME_HEADER.size])
                if length == 0:
                    raise SeverinTransportError("zero-length bridge frame")
                if length > self.max_frame_bytes:
                    raise SeverinTransportError("oversized bridge frame")
                if len(buffer) < _FRAME_HEADER.size + length:
                    break
                payload = bytes(buffer[_FRAME_HEADER.size:_FRAME_HEADER.size + length])
                del buffer[:_FRAME_HEADER.size + length]
                try:
                    json_text = payload.decode("utf-8")
                except UnicodeDecodeError as exc:
                    raise SeverinTransportError("bridge frame is not UTF-8") from exc
                self._validate_json(json_text)
                self._dispatch(receipt, json_text)

    def _dispatch(self, receipt: int, json_text: str) -> None:
        if self.bridge is None:
            return
        try:
            reply = self.bridge(receipt, json_text)
        except Exception:  # Transport-neutral: do not invent application JSON.
            logging.exception("Severin bridge callback failed for receipt %s", receipt)
            return
        if reply is None:
            return
        if not isinstance(reply, str):
            logging.error(
                "Severin bridge callback returned non-str for receipt %s; leaving unresolved",
                receipt,
            )
            return
        self.write(receipt, reply)

    def _writer_main(self) -> None:
        fd = self._reply_fd
        if fd is None:
            return
        while not self._closed.is_set():
            item = self._write_queue.get()
            if item is None:
                return
            receipt, json_text = item
            frame = self._encode_frame(receipt, json_text)
            try:
                self._write_all(fd, frame)
            except OSError as exc:
                self._run_error = SeverinTransportError(f"reply pipe write failed: {exc}")
                self._closed.set()
                return

    def _encode_frame(self, receipt: int, json_text: str) -> bytes:
        payload = json_text.encode("utf-8")
        if not payload or len(payload) > self.max_frame_bytes:
            raise SeverinTransportError("invalid reply frame size")
        return _FRAME_HEADER.pack(len(payload), receipt) + payload

    @staticmethod
    def _write_all(fd: int, data: bytes) -> None:
        view = memoryview(data)
        offset = 0
        while offset < len(view):
            written = os.write(fd, view[offset:])
            if written == 0:
                raise BrokenPipeError("zero-byte pipe write")
            offset += written

    def _validate_json(self, json_text: str) -> None:
        if len(json_text.encode("utf-8")) > self.max_frame_bytes:
            raise SeverinTransportError("JSON frame exceeds max_frame_bytes")
        json.loads(json_text)
