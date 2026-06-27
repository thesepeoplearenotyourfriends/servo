/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Optional Severin inherited-FD bridge for the headed desktop shell.
//!
//! This is a private receipt mailbox. It is not a resource loader, RPC schema,
//! listener, or application protocol.

use std::collections::{HashMap, VecDeque};
use std::io::{ErrorKind, Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::sync::mpsc::{
    Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError, channel, sync_channel,
};
use std::thread;
use std::time::{Duration, Instant};

use log::{debug, warn};
use servo::{JSValue, JavaScriptEvaluationError, UserScript, WebView, WebViewId};
use winit::event_loop::EventLoopProxy;

use super::event_loop::AppEvent;
use crate::prefs::{BridgeFdConfig, BridgeTimingConfig};

const FRAME_HEADER_BYTES: usize = 12;
const READ_CHUNK_BYTES: usize = 8192;

#[derive(Clone, Debug)]
pub(crate) struct BridgeLimits {
    pub max_frame_bytes: usize,
    pub max_live_receipts: usize,
    pub max_queued_frames: usize,
    pub max_queued_bytes: usize,
    pub messages_per_second: u64,
    pub message_burst: u64,
    pub bytes_per_second: u64,
    pub byte_burst: u64,
    pub max_deliveries_per_pump: usize,
}

impl Default for BridgeLimits {
    fn default() -> Self {
        Self {
            max_frame_bytes: 1024 * 1024,
            max_live_receipts: 128,
            max_queued_frames: 128,
            max_queued_bytes: 8 * 1024 * 1024,
            messages_per_second: 256,
            message_burst: 128,
            bytes_per_second: 8 * 1024 * 1024,
            byte_burst: 2 * 1024 * 1024,
            max_deliveries_per_pump: 32,
        }
    }
}

#[derive(Default)]
struct BridgeLedger {
    frames_rejected_too_large: u64,
    frames_rejected_backpressure: u64,
    frames_rejected_rate: u64,
    stale_replies_rejected: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct BridgeFrame {
    pub receipt: u64,
    pub json: String,
}

#[derive(Debug)]
pub(crate) enum BridgeThreadEvent {
    Reply(BridgeFrame),
    Closed(String),
    PollDrain(u64),
}

pub(crate) enum BridgeEventOutcome {
    None,
    DeliverReply(WebViewId, String),
    CloseShell,
}

struct PendingReplyTarget {
    webview_id: WebViewId,
    document_id: String,
    call_id: u64,
}

pub(crate) struct BridgeTransport {
    limits: BridgeLimits,
    next_receipt: u64,
    inbound: VecDeque<BridgeFrame>,
    pending: HashMap<u64, PendingReplyTarget>,
    active_document_id: Option<String>,
    queued_byte_count: usize,
    message_tokens: f64,
    byte_tokens: f64,
    last_refill: Instant,
    ledger: BridgeLedger,
}

impl BridgeTransport {
    fn new(limits: BridgeLimits) -> Self {
        Self {
            message_tokens: limits.message_burst as f64,
            byte_tokens: limits.byte_burst as f64,
            last_refill: Instant::now(),
            limits,
            next_receipt: 0,
            inbound: VecDeque::new(),
            pending: HashMap::new(),
            active_document_id: None,
            queued_byte_count: 0,
            ledger: BridgeLedger::default(),
        }
    }

    fn refill(&mut self) {
        let elapsed = self.last_refill.elapsed();
        self.last_refill = Instant::now();
        let secs = elapsed.as_secs_f64();
        self.message_tokens = (self.message_tokens + secs * self.limits.messages_per_second as f64)
            .min(self.limits.message_burst as f64);
        self.byte_tokens = (self.byte_tokens + secs * self.limits.bytes_per_second as f64)
            .min(self.limits.byte_burst as f64);
    }

    fn enqueue_from_javascript(
        &mut self,
        webview_id: WebViewId,
        document_id: String,
        call_id: u64,
        json: String,
    ) -> Result<BridgeFrame, &'static str> {
        let bytes = json.len();
        if bytes > self.limits.max_frame_bytes {
            self.ledger.frames_rejected_too_large += 1;
            return Err("BridgeTooLargeError");
        }
        validate_json_frame(&json).map_err(|_| "BridgeBackpressureError")?;
        let Some(new_queued_byte_count) = self.queued_byte_count.checked_add(bytes) else {
            self.ledger.frames_rejected_backpressure += 1;
            return Err("BridgeBackpressureError");
        };
        if self.pending.len() >= self.limits.max_live_receipts
            || self.inbound.len() >= self.limits.max_queued_frames
            || new_queued_byte_count > self.limits.max_queued_bytes
        {
            self.ledger.frames_rejected_backpressure += 1;
            return Err("BridgeBackpressureError");
        }
        self.refill();
        if self.message_tokens < 1.0 || self.byte_tokens < bytes as f64 {
            self.ledger.frames_rejected_rate += 1;
            return Err("BridgeBackpressureError");
        }
        self.message_tokens -= 1.0;
        self.byte_tokens -= bytes as f64;
        self.active_document_id = Some(document_id.clone());
        self.next_receipt = self.next_receipt.checked_add(1).ok_or_else(|| {
            self.ledger.frames_rejected_backpressure += 1;
            "BridgeBackpressureError"
        })?;
        let receipt = self.next_receipt;
        self.pending.insert(
            receipt,
            PendingReplyTarget {
                webview_id,
                document_id,
                call_id,
            },
        );
        self.queued_byte_count = new_queued_byte_count;
        let frame = BridgeFrame { receipt, json };
        self.inbound.push_back(frame.clone());
        Ok(frame)
    }

    fn pop_queued_frame(&mut self) -> Option<BridgeFrame> {
        let frame = self.inbound.pop_front()?;
        self.queued_byte_count = self.queued_byte_count.saturating_sub(frame.json.len());
        Some(frame)
    }

    fn prepare_reply(&mut self, receipt: u64, json: &str) -> Result<(WebViewId, String), String> {
        if json.is_empty() {
            return Err("bridge reply frame is empty".to_owned());
        }
        if json.len() > self.limits.max_frame_bytes {
            return Err("bridge reply exceeds max_frame_bytes".to_owned());
        }
        validate_json_frame(json)?;
        let Some(target) = self.pending.get(&receipt) else {
            self.ledger.stale_replies_rejected += 1;
            return Err("bridge reply target no longer exists".to_owned());
        };
        if self.active_document_id.as_deref() != Some(target.document_id.as_str()) {
            self.ledger.stale_replies_rejected += 1;
            return Err("bridge reply target document is no longer active".to_owned());
        }
        let webview_id = target.webview_id;
        let script = resolve_script(&target.document_id, target.call_id, json);
        self.pending.remove(&receipt);
        Ok((webview_id, script))
    }

    fn finish_reply_delivery(&mut self, delivered: bool) -> Result<(), String> {
        if delivered {
            Ok(())
        } else {
            Err("bridge reply target was not found in the active document".to_owned())
        }
    }

    fn take_rejection_script(&mut self, receipt: u64, name: &str) -> Option<String> {
        let target = self.pending.remove(&receipt)?;
        Some(reject_script(&target.document_id, target.call_id, name))
    }

    fn clear(&mut self) {
        self.inbound.clear();
        self.pending.clear();
        self.active_document_id = None;
        self.queued_byte_count = 0;
        self.message_tokens = self.limits.message_burst as f64;
        self.byte_tokens = self.limits.byte_burst as f64;
        self.last_refill = Instant::now();
    }

    fn observe_document(&mut self, document_id: &str) -> bool {
        match self.active_document_id.as_deref() {
            None => {
                self.active_document_id = Some(document_id.to_owned());
                false
            },
            Some(active) if active == document_id => false,
            Some(_) => {
                self.clear();
                self.active_document_id = Some(document_id.to_owned());
                true
            },
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum PendingEvaluationKind {
    DrainOutbound,
    DeliverReply,
    RejectRequest,
}

struct PendingEvaluation {
    kind: PendingEvaluationKind,
    result: std::rc::Rc<std::cell::RefCell<Option<Result<JSValue, JavaScriptEvaluationError>>>>,
}

struct DrainTiming {
    idle_poll: Duration,
    busy_poll: Duration,
    startup_retry_delays: Vec<Duration>,
}

impl From<BridgeTimingConfig> for DrainTiming {
    fn from(config: BridgeTimingConfig) -> Self {
        Self {
            idle_poll: Duration::from_millis(config.idle_poll_ms),
            busy_poll: Duration::from_millis(config.busy_poll_ms),
            startup_retry_delays: config
                .startup_retry_ms
                .into_iter()
                .map(Duration::from_millis)
                .collect(),
        }
    }
}

enum DrainTimerCommand {
    Schedule { generation: u64, delay: Duration },
}

pub(crate) struct SeverinBridge {
    transport: BridgeTransport,
    writer: Option<SyncSender<BridgeFrame>>,
    drain_timer: Option<Sender<DrainTimerCommand>>,
    timing: DrainTiming,
    pending_evaluations: Vec<PendingEvaluation>,
    drain_generation: u64,
    drain_due: bool,
    startup_retry_index: usize,
    drain_polling_disabled: bool,
    closed: bool,
}

impl SeverinBridge {
    pub(crate) fn new(config: BridgeFdConfig, proxy: EventLoopProxy<AppEvent>) -> Self {
        let BridgeFdConfig {
            request_fd,
            reply_fd,
            timing,
        } = config;

        set_close_on_exec(request_fd);
        set_close_on_exec(reply_fd);

        let limits = BridgeLimits::default();
        let (writer_tx, writer_rx) = sync_channel(limits.max_queued_frames);
        let (timer_tx, timer_rx) = channel();

        spawn_drain_timer(timer_rx, proxy.clone());
        spawn_reply_reader(
            reply_fd,
            limits.max_frame_bytes,
            limits.max_queued_bytes,
            proxy.clone(),
        );
        spawn_request_writer(request_fd, writer_rx, proxy);

        let mut bridge = Self {
            transport: BridgeTransport::new(limits),
            writer: Some(writer_tx),
            drain_timer: Some(timer_tx),
            timing: DrainTiming::from(timing),
            pending_evaluations: Vec::new(),
            drain_generation: 0,
            drain_due: false,
            startup_retry_index: 0,
            drain_polling_disabled: false,
            closed: false,
        };
        bridge.schedule_drain_after(Duration::ZERO);
        bridge
    }

    pub(crate) fn user_script() -> UserScript {
        UserScript::from(bridge_shim(&BridgeLimits::default()))
    }

    pub(crate) fn clear_for_navigation(&mut self) {
        self.pending_evaluations.clear();
        self.transport.clear();
        self.reset_drain_state();
        self.schedule_drain_after(Duration::ZERO);
    }

    pub(crate) fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.writer.take();
        self.drain_timer.take();
        self.pending_evaluations.clear();
        self.transport.clear();
        self.drain_generation = self.drain_generation.wrapping_add(1);
        self.drain_due = false;
    }

    pub(crate) fn handle_thread_event(
        &mut self,
        event: BridgeThreadEvent,
    ) -> Result<BridgeEventOutcome, String> {
        match event {
            BridgeThreadEvent::Reply(frame) => {
                if self.closed {
                    return Ok(BridgeEventOutcome::None);
                }
                let (webview_id, script) =
                    self.transport.prepare_reply(frame.receipt, &frame.json)?;
                Ok(BridgeEventOutcome::DeliverReply(webview_id, script))
            },
            BridgeThreadEvent::Closed(reason) => {
                warn!("Severin bridge closed: {reason}");
                self.close();
                Ok(BridgeEventOutcome::CloseShell)
            },
            BridgeThreadEvent::PollDrain(generation) => {
                if !self.closed
                    && !self.drain_polling_disabled
                    && generation == self.drain_generation
                {
                    self.drain_due = true;
                }
                Ok(BridgeEventOutcome::None)
            },
        }
    }

    pub(crate) fn schedule_reply_delivery(&mut self, webview: &WebView, script: String) {
        self.schedule_evaluation(webview, script, PendingEvaluationKind::DeliverReply);
    }

    pub(crate) fn pump(&mut self, webview: Option<WebView>) -> Result<(), String> {
        let Some(webview) = webview else {
            return Ok(());
        };
        self.collect_bridge_evaluations(&webview)?;
        if !self.closed
            && !self.drain_polling_disabled
            && self.drain_due
            && !self.has_pending_drain()
        {
            self.drain_due = false;
            self.schedule_outbound_drain(&webview);
        }
        Ok(())
    }

    fn has_pending_drain(&self) -> bool {
        self.pending_evaluations
            .iter()
            .any(|evaluation| matches!(evaluation.kind, PendingEvaluationKind::DrainOutbound))
    }

    fn schedule_outbound_drain(&mut self, webview: &WebView) {
        if self.has_pending_drain() {
            return;
        }
        let result = std::rc::Rc::new(std::cell::RefCell::new(None));
        let callback_result = result.clone();
        webview.evaluate_javascript(
            drain_script(self.transport.limits.max_deliveries_per_pump),
            move |value| {
                *callback_result.borrow_mut() = Some(value);
            },
        );
        self.pending_evaluations.push(PendingEvaluation {
            kind: PendingEvaluationKind::DrainOutbound,
            result,
        });
    }

    fn schedule_evaluation(
        &mut self,
        webview: &WebView,
        script: String,
        kind: PendingEvaluationKind,
    ) {
        let result = std::rc::Rc::new(std::cell::RefCell::new(None));
        let callback_result = result.clone();
        webview.evaluate_javascript(script, move |value| {
            *callback_result.borrow_mut() = Some(value);
        });
        self.pending_evaluations
            .push(PendingEvaluation { kind, result });
    }

    fn collect_bridge_evaluations(&mut self, webview: &WebView) -> Result<(), String> {
        let mut index = 0;
        while index < self.pending_evaluations.len() {
            let result = { self.pending_evaluations[index].result.borrow_mut().take() };
            let Some(result) = result else {
                index += 1;
                continue;
            };
            let evaluation = self.pending_evaluations.remove(index);
            match evaluation.kind {
                PendingEvaluationKind::DrainOutbound => {
                    self.handle_drain_result(webview, result)?
                },
                PendingEvaluationKind::DeliverReply => {
                    let delivered = match result {
                        Ok(JSValue::Boolean(true)) => true,
                        Ok(value) => {
                            return Err(format!(
                                "Severin bridge reply evaluation returned unexpected value: {value:?}"
                            ));
                        },
                        Err(error) => {
                            return Err(format!(
                                "Severin bridge reply evaluation failed: {error:?}"
                            ));
                        },
                    };
                    self.transport.finish_reply_delivery(delivered)?;
                },
                PendingEvaluationKind::RejectRequest => match result {
                    Ok(JSValue::Boolean(_)) => {},
                    Ok(value) => {
                        return Err(format!(
                            "Severin bridge request rejection returned unexpected value: {value:?}"
                        ));
                    },
                    Err(error) => {
                        return Err(format!(
                            "Severin bridge request rejection failed: {error:?}"
                        ));
                    },
                },
            }
        }
        Ok(())
    }

    fn handle_drain_result(
        &mut self,
        webview: &WebView,
        result: Result<JSValue, JavaScriptEvaluationError>,
    ) -> Result<(), String> {
        let serialized = match result {
            Ok(JSValue::String(serialized)) => serialized,
            Ok(value) => {
                return Err(format!(
                    "Severin bridge drain evaluation returned unexpected value: {value:?}"
                ));
            },
            Err(error) => {
                self.schedule_startup_retry(format!(
                    "JavaScript evaluation failed before the bridge drain completed: {error:?}"
                ));
                return Ok(());
            },
        };
        let drained: serde_json::Value = serde_json::from_str(&serialized)
            .map_err(|error| format!("invalid Severin bridge drain result: {error}"))?;
        if let Some(delta) = drained.get("rejectionDelta") {
            if let Some(too_large) = delta.get("tooLarge").and_then(|value| value.as_u64()) {
                self.transport.ledger.frames_rejected_too_large = self
                    .transport
                    .ledger
                    .frames_rejected_too_large
                    .saturating_add(too_large);
            }
            if let Some(backpressure) = delta.get("backpressure").and_then(|value| value.as_u64()) {
                self.transport.ledger.frames_rejected_backpressure = self
                    .transport
                    .ledger
                    .frames_rejected_backpressure
                    .saturating_add(backpressure);
            }
        }

        let document_id = drained
            .get("documentId")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_owned();
        if document_id.is_empty() {
            self.schedule_startup_retry(
                "the bridge drain helper is not present in the current JavaScript realm".to_owned(),
            );
            return Ok(());
        }

        let document_changed = self.transport.observe_document(&document_id);
        if document_changed {
            self.pending_evaluations.clear();
            self.reset_drain_state();
        }
        self.reset_startup_retry_after_success();

        let Some(frames) = drained.get("frames").and_then(|value| value.as_array()) else {
            return Err("Severin bridge drain result did not contain a frames array".to_owned());
        };
        let queued_remaining = drained
            .get("queuedRemaining")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);

        let webview_id = webview.id();
        for frame in frames {
            let Some(call_id) = frame.get("callId").and_then(|value| value.as_u64()) else {
                continue;
            };
            let Some(json) = frame.get("json").and_then(|value| value.as_str()) else {
                continue;
            };
            let outbound = match self.transport.enqueue_from_javascript(
                webview_id,
                document_id.clone(),
                call_id,
                json.to_owned(),
            ) {
                Ok(frame) => frame,
                Err(error_name) => {
                    self.schedule_evaluation(
                        webview,
                        reject_script(&document_id, call_id, error_name),
                        PendingEvaluationKind::RejectRequest,
                    );
                    continue;
                },
            };
            let Some(writer) = &self.writer else {
                continue;
            };
            match writer.try_send(outbound) {
                Ok(()) => {
                    let _ = self.transport.pop_queued_frame();
                },
                Err(TrySendError::Full(frame)) => {
                    self.transport.ledger.frames_rejected_backpressure += 1;
                    if let Some(script) = self
                        .transport
                        .take_rejection_script(frame.receipt, "BridgeBackpressureError")
                    {
                        self.schedule_evaluation(
                            webview,
                            script,
                            PendingEvaluationKind::RejectRequest,
                        );
                    }
                    let _ = self.transport.pop_queued_frame();
                    break;
                },
                Err(TrySendError::Disconnected(_)) => {
                    self.close();
                    break;
                },
            }
        }

        if !self.closed && !self.drain_polling_disabled {
            let delay = if queued_remaining > 0 {
                self.timing.busy_poll
            } else {
                self.timing.idle_poll
            };
            self.schedule_drain_after(delay);
        }
        Ok(())
    }

    fn reset_drain_state(&mut self) {
        self.drain_generation = self.drain_generation.wrapping_add(1);
        self.drain_due = false;
        self.startup_retry_index = 0;
        self.drain_polling_disabled = false;
    }

    fn reset_startup_retry_after_success(&mut self) {
        self.startup_retry_index = 0;
        self.drain_polling_disabled = false;
    }

    fn schedule_startup_retry(&mut self, reason: String) {
        if self.closed || self.drain_polling_disabled {
            return;
        }

        let retry_number = self.startup_retry_index + 1;
        let Some(delay) = self
            .timing
            .startup_retry_delays
            .get(self.startup_retry_index)
            .copied()
        else {
            self.drain_polling_disabled = true;
            warn!(
                "Severin bridge outbound drain remained unavailable after {} bounded retries; \
                 automatic drain polling is disabled for this document while the window remains open: {}",
                self.startup_retry_index,
                reason
            );
            return;
        };

        self.startup_retry_index = retry_number;
        debug!(
            "Severin bridge outbound drain unavailable; retry {}/{} in {:?}: {}",
            retry_number,
            self.timing.startup_retry_delays.len(),
            delay,
            reason
        );
        self.schedule_drain_after(delay);
    }

    fn schedule_drain_after(&mut self, delay: Duration) {
        if self.closed || self.drain_polling_disabled {
            return;
        }

        self.drain_generation = self.drain_generation.wrapping_add(1);
        self.drain_due = false;
        let generation = self.drain_generation;
        let Some(timer) = &self.drain_timer else {
            self.drain_polling_disabled = true;
            warn!(
                "Severin bridge drain timer is unavailable; automatic drain polling is disabled \
                 while the window remains open"
            );
            return;
        };
        if timer
            .send(DrainTimerCommand::Schedule { generation, delay })
            .is_err()
        {
            self.drain_polling_disabled = true;
            warn!(
                "Severin bridge drain timer stopped unexpectedly; automatic drain polling is \
                 disabled while the window remains open"
            );
        }
    }
}

fn spawn_drain_timer(rx: Receiver<DrainTimerCommand>, proxy: EventLoopProxy<AppEvent>) {
    thread::Builder::new()
        .name("severin-bridge-drain-timer".to_owned())
        .spawn(move || {
            let mut scheduled: Option<(u64, Instant)> = None;
            loop {
                if let Some((generation, deadline)) = scheduled {
                    let wait = deadline.saturating_duration_since(Instant::now());
                    match rx.recv_timeout(wait) {
                        Ok(DrainTimerCommand::Schedule { generation, delay }) => {
                            scheduled = Some((generation, Instant::now() + delay));
                        },
                        Err(RecvTimeoutError::Timeout) => {
                            scheduled = None;
                            if proxy
                                .send_event(AppEvent::SeverinBridge(BridgeThreadEvent::PollDrain(
                                    generation,
                                )))
                                .is_err()
                            {
                                return;
                            }
                        },
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                } else {
                    match rx.recv() {
                        Ok(DrainTimerCommand::Schedule { generation, delay }) => {
                            scheduled = Some((generation, Instant::now() + delay));
                        },
                        Err(_) => return,
                    }
                }
            }
        })
        .expect("failed to spawn Severin bridge drain timer");
}

fn set_close_on_exec(fd: RawFd) {
    // Best-effort: these are inherited child-side bridge FDs, but they should not leak
    // into any later program this process might spawn.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags < 0 {
            warn!("failed to read bridge FD flags for fd {fd}");
            return;
        }
        if libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) < 0 {
            warn!("failed to mark bridge fd {fd} close-on-exec");
        }
    }
}

fn spawn_reply_reader(
    fd: RawFd,
    max_frame_bytes: usize,
    max_buffer_bytes: usize,
    proxy: EventLoopProxy<AppEvent>,
) {
    thread::Builder::new()
        .name("severin-bridge-reply-reader".to_owned())
        .spawn(move || {
            // SAFETY: ownership of this inherited child-side FD is transferred to the reader thread once.
            let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
            let mut parser = FrameParser::new(max_frame_bytes, max_buffer_bytes);
            let mut chunk = [0_u8; READ_CHUNK_BYTES];
            loop {
                match file.read(&mut chunk) {
                    Ok(0) => {
                        let _ = proxy.send_event(AppEvent::SeverinBridge(
                            BridgeThreadEvent::Closed("parent reply pipe reached EOF".to_owned()),
                        ));
                        break;
                    },
                    Ok(count) => match parser.push(&chunk[..count]) {
                        Ok(frames) => {
                            for frame in frames {
                                if proxy
                                    .send_event(AppEvent::SeverinBridge(BridgeThreadEvent::Reply(
                                        frame,
                                    )))
                                    .is_err()
                                {
                                    return;
                                }
                            }
                        },
                        Err(error) => {
                            let _ = proxy.send_event(AppEvent::SeverinBridge(
                                BridgeThreadEvent::Closed(error),
                            ));
                            break;
                        },
                    },
                    Err(error) if error.kind() == ErrorKind::Interrupted => {},
                    Err(error) => {
                        let _ = proxy.send_event(AppEvent::SeverinBridge(
                            BridgeThreadEvent::Closed(format!("reply pipe read failed: {error}")),
                        ));
                        break;
                    },
                }
            }
        })
        .expect("failed to spawn Severin bridge reader");
}

fn spawn_request_writer(fd: RawFd, rx: Receiver<BridgeFrame>, proxy: EventLoopProxy<AppEvent>) {
    thread::Builder::new()
        .name("severin-bridge-request-writer".to_owned())
        .spawn(move || {
            // SAFETY: ownership of this inherited child-side FD is transferred to the writer thread once.
            let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
            while let Ok(frame) = rx.recv() {
                let bytes = encode_frame(&frame);
                if let Err(error) = write_all_interruptible(&mut file, &bytes) {
                    let _ = proxy.send_event(AppEvent::SeverinBridge(BridgeThreadEvent::Closed(
                        format!("request pipe write failed: {error}"),
                    )));
                    return;
                }
            }
            debug!("Severin bridge request writer exiting");
        })
        .expect("failed to spawn Severin bridge writer");
}

fn write_all_interruptible(writer: &mut std::fs::File, mut bytes: &[u8]) -> std::io::Result<()> {
    while !bytes.is_empty() {
        match writer.write(bytes) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    ErrorKind::WriteZero,
                    "zero-byte pipe write",
                ));
            },
            Ok(count) => bytes = &bytes[count..],
            Err(error) if error.kind() == ErrorKind::Interrupted => {},
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn encode_frame(frame: &BridgeFrame) -> Vec<u8> {
    let json = frame.json.as_bytes();
    let mut bytes = Vec::with_capacity(FRAME_HEADER_BYTES + json.len());
    bytes.extend_from_slice(&(json.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&frame.receipt.to_be_bytes());
    bytes.extend_from_slice(json);
    bytes
}

struct FrameParser {
    max_frame_bytes: usize,
    max_buffer_bytes: usize,
    buffer: Vec<u8>,
}

impl FrameParser {
    fn new(max_frame_bytes: usize, max_buffer_bytes: usize) -> Self {
        Self {
            max_frame_bytes,
            max_buffer_bytes,
            buffer: Vec::new(),
        }
    }

    fn push(&mut self, bytes: &[u8]) -> Result<Vec<BridgeFrame>, String> {
        if self.buffer.len().saturating_add(bytes.len())
            > self.max_buffer_bytes + FRAME_HEADER_BYTES
        {
            return Err("bridge parser buffer exceeded max frame envelope".to_owned());
        }
        self.buffer.extend_from_slice(bytes);
        let mut frames = Vec::new();
        loop {
            if self.buffer.len() < FRAME_HEADER_BYTES {
                break;
            }
            let len = u32::from_be_bytes(self.buffer[0..4].try_into().unwrap()) as usize;
            let receipt = u64::from_be_bytes(self.buffer[4..12].try_into().unwrap());
            if len == 0 {
                return Err("bridge frame has zero-length JSON payload".to_owned());
            }
            if len > self.max_frame_bytes {
                return Err(format!("bridge frame exceeds max_frame_bytes: {len}"));
            }
            let frame_len = FRAME_HEADER_BYTES + len;
            if self.buffer.len() < frame_len {
                break;
            }
            let payload = self.buffer[FRAME_HEADER_BYTES..frame_len].to_vec();
            self.buffer.drain(..frame_len);
            let json = String::from_utf8(payload)
                .map_err(|error| format!("bridge frame is not UTF-8: {error}"))?;
            validate_json_frame(&json)?;
            frames.push(BridgeFrame { receipt, json });
        }
        Ok(frames)
    }
}

fn validate_json_frame(json: &str) -> Result<(), String> {
    if json.is_empty() {
        return Err("empty JSON bridge frame".to_owned());
    }
    serde_json::from_str::<serde_json::Value>(json)
        .map(|_| ())
        .map_err(|error| format!("invalid JSON bridge frame: {error}"))
}

const SEVERIN_BRIDGE_SHIM: &str = r#"
(() => {
  const MAX_FRAME_BYTES = __SEVERIN_MAX_FRAME_BYTES__;
  const MAX_LIVE_RECEIPTS = __SEVERIN_MAX_LIVE_RECEIPTS__;
  const MAX_QUEUED_FRAMES = __SEVERIN_MAX_QUEUED_FRAMES__;
  const MAX_QUEUED_BYTES = __SEVERIN_MAX_QUEUED_BYTES__;
  const documentId = `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
  let nextCallId = 1;
  const outbound = [];
  const pending = new Map();
  let outboundByteCount = 0;
  let rejectedTooLarge = 0;
  let rejectedBackpressure = 0;

  function rejectNotJson() {
    return Promise.reject(new TypeError("severin.send(value) requires a strict JSON value"));
  }

  function rejectTransport(name) {
    const error = new Error(name);
    error.name = name;
    return Promise.reject(error);
  }

  function utf8ByteLength(source) {
    return new TextEncoder().encode(source).length;
  }

  function send(value) {
    let json;
    try { json = JSON.stringify(value); } catch (_) { return rejectNotJson(); }
    if (typeof json !== "string") { return rejectNotJson(); }
    const jsonBytes = utf8ByteLength(json);
    if (jsonBytes > MAX_FRAME_BYTES) {
      rejectedTooLarge += 1;
      return rejectTransport("BridgeTooLargeError");
    }
    if (pending.size >= MAX_LIVE_RECEIPTS || outbound.length >= MAX_QUEUED_FRAMES || outboundByteCount + jsonBytes > MAX_QUEUED_BYTES) {
      rejectedBackpressure += 1;
      return rejectTransport("BridgeBackpressureError");
    }
    const callId = nextCallId++;
    outbound.push({ callId, json, jsonBytes });
    outboundByteCount += jsonBytes;
    return new Promise((resolve, reject) => { pending.set(callId, { resolve, reject }); });
  }

  Object.defineProperty(globalThis, "severin", { value: Object.freeze({ send }), configurable: false, enumerable: false, writable: false });
  Object.defineProperty(globalThis, "__severinDrain", { value(limit) {
    const drained = outbound.splice(0, limit);
    for (const frame of drained) { outboundByteCount -= frame.jsonBytes; delete frame.jsonBytes; }
    const rejectionDelta = { tooLarge: rejectedTooLarge, backpressure: rejectedBackpressure };
    rejectedTooLarge = 0; rejectedBackpressure = 0;
    return { documentId, frames: drained, queuedRemaining: outbound.length, rejectionDelta };
  }, configurable: false, enumerable: false, writable: false });
  Object.defineProperty(globalThis, "__severinReject", { value(expectedDocumentId, callId, name) {
    if (expectedDocumentId !== documentId) { return false; }
    const target = pending.get(callId);
    if (!target) { return false; }
    const error = new Error(name); error.name = name; pending.delete(callId); target.reject(error); return true;
  }, configurable: false, enumerable: false, writable: false });
  Object.defineProperty(globalThis, "__severinResolve", { value(expectedDocumentId, callId, jsonSource) {
    if (expectedDocumentId !== documentId) { return false; }
    const target = pending.get(callId);
    if (!target) { return false; }
    let value;
    try { value = JSON.parse(jsonSource); } catch (error) { target.reject(error); pending.delete(callId); return false; }
    pending.delete(callId); target.resolve(value); return true;
  }, configurable: false, enumerable: false, writable: false });
})();
"#;

const DRAIN_SCRIPT: &str = r#"
(() => {
  if (typeof globalThis.__severinDrain !== "function") {
    return JSON.stringify({ documentId: null, frames: [] });
  }
  return JSON.stringify(globalThis.__severinDrain(__SEVERIN_DRAIN_LIMIT__));
})()
"#;

fn bridge_shim(limits: &BridgeLimits) -> String {
    SEVERIN_BRIDGE_SHIM
        .replace(
            "__SEVERIN_MAX_FRAME_BYTES__",
            &limits.max_frame_bytes.to_string(),
        )
        .replace(
            "__SEVERIN_MAX_LIVE_RECEIPTS__",
            &limits.max_live_receipts.to_string(),
        )
        .replace(
            "__SEVERIN_MAX_QUEUED_FRAMES__",
            &limits.max_queued_frames.to_string(),
        )
        .replace(
            "__SEVERIN_MAX_QUEUED_BYTES__",
            &limits.max_queued_bytes.to_string(),
        )
}

fn drain_script(limit: usize) -> String {
    DRAIN_SCRIPT.replace("__SEVERIN_DRAIN_LIMIT__", &limit.to_string())
}

fn reject_script(document_id: &str, call_id: u64, name: &str) -> String {
    let document_id_literal = serde_json::to_string(document_id).expect("document id serializes");
    let name_literal = serde_json::to_string(name).expect("error name serializes");
    format!(
        r#"(() => {{
  if (typeof globalThis.__severinReject !== "function") {{ return false; }}
  return globalThis.__severinReject({document_id_literal}, {call_id}, {name_literal});
}})()"#
    )
}

fn resolve_script(document_id: &str, call_id: u64, json: &str) -> String {
    let document_id_literal = serde_json::to_string(document_id).expect("document id serializes");
    let json_literal = serde_json::to_string(json).expect("JSON source string serializes");
    format!(
        r#"(() => {{
  if (typeof globalThis.__severinResolve !== "function") {{ return false; }}
  return globalThis.__severinResolve({document_id_literal}, {call_id}, {json_literal});
}})()"#
    )
}
