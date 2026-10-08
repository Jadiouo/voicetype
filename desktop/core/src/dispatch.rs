//! Native Linux dispatch. This code runs on a worker owned by the app. It never
//! owns daily service endpoints: the caller supplies an exclusively owned local
//! engine connection and a peer accepted on the app's private frontend endpoint.
use crate::{
    local::{deadline, invalid, CorrectionResult, LocalConnection, Wire},
    Application, DeliveryOutcome, DeliveryPort, DictationContext, ProviderEvent, SessionFailure,
    SessionKey, TargetLease,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    io,
    os::unix::net::UnixStream,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

static NEXT_HELLO: AtomicU64 = AtomicU64::new(1);
const IO_BUDGET: Duration = Duration::from_millis(300);

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum InputCommand {
    Start {
        session: u64,
        context_id: String,
        is_password: bool,
        #[serde(flatten)]
        context: DictationContext,
    },
    Stop {
        session: u64,
    },
    Cancel {
        session: u64,
    },
    Correction {
        session: u64,
        context_id: String,
        program: String,
        before: String,
        after: String,
        #[serde(default)]
        confirmed: bool,
    },
}

struct RouteSession {
    frontend: u64,
    key: SessionKey,
    target: TargetLease,
    program: String,
}

pub struct LocalDispatcher {
    input: FcitxConnection,
    engine: LocalConnection,
    active: Option<RouteSession>,
    last_session: u64,
    last_delivered: Option<RouteSession>,
    failed: bool,
}

impl LocalDispatcher {
    /// Authenticate and negotiate without starting capture. The caller retains
    /// the actual child handle; an error is never proof of microphone release.
    pub fn accept(peer: UnixStream, engine: LocalConnection, budget: Duration) -> io::Result<Self> {
        Ok(Self {
            input: FcitxConnection::accept(peer, budget)?,
            engine,
            active: None,
            last_session: 0,
            last_delivered: None,
            failed: false,
        })
    }

    /// Process input before engine events so queued cancellation invalidates a
    /// final result. Call repeatedly off the UI thread; normal idle polling is
    /// capped at 10 ms. Exceptional command/delivery IO is separately bounded.
    pub fn step(&mut self, app: &mut Application, budget: Duration) -> io::Result<()> {
        if self.failed {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let result = self.step_inner(app, budget.min(Duration::from_millis(10)));
        if result.is_err() {
            self.fail(app);
        }
        result
    }

    pub(crate) fn fail(&mut self, app: &mut Application) {
        self.failed = true;
        self.input.target = None;
        if let Some(active) = &self.active {
            app.provider_event(
                active.key,
                ProviderEvent::Failed(SessionFailure::ProviderFailed),
                &mut self.input,
            );
            let _ = app.cancel_dictation(&mut self.engine);
        }
    }

    fn step_inner(&mut self, app: &mut Application, budget: Duration) -> io::Result<()> {
        self.enforce_deadline(app)?;
        let mut input_budget = budget / 2;
        let mut drained = false;
        for _ in 0..32 {
            let Some(command) = self.input.poll(input_budget)? else {
                drained = true;
                break;
            };
            input_budget = Duration::from_millis(1);
            match command {
                InputCommand::Start {
                    session,
                    context_id,
                    is_password,
                    context,
                } => {
                    if session == 0 || session <= self.last_session {
                        // Never let a replayed Start cancel or replace its owner.
                        continue;
                    }
                    self.last_session = session;
                    if self.active.is_some() || app.snapshot().dictation.busy {
                        if self.active.is_some() {
                            app.cancel_dictation(&mut self.engine)
                                .map_err(io::Error::other)?;
                        }
                        self.input.error(session, "請等待目前工作結束後再錄音")?;
                    } else if is_password
                        || context_id.is_empty()
                        || context_id.len() > 640
                        || context_id.contains('\0')
                        || context.program.len() > 512
                        || context.context_text.len() > 16384
                        || context.selected_text.len() > 2048
                    {
                        self.input.error(session, "此欄位無法使用語音輸入")?;
                    } else {
                        let target = TargetLease::new(context_id, session);
                        let program = context.program.clone();
                        match app.start_dictation_with_context(
                            target.clone(),
                            context,
                            &mut self.engine,
                        ) {
                            Ok(key) => {
                                self.input.target = Some(target.clone());
                                self.active = Some(RouteSession {
                                    frontend: session,
                                    key,
                                    target,
                                    program,
                                });
                            }
                            Err(_) => self.input.error(session, "所選辨識引擎尚未就緒")?,
                        }
                    }
                }
                InputCommand::Stop { session } if self.matches(session) => {
                    app.stop_dictation(&mut self.engine)
                        .map_err(io::Error::other)?;
                }
                InputCommand::Cancel { session } if self.matches(session) => {
                    self.input.target = None;
                    app.cancel_dictation(&mut self.engine)
                        .map_err(io::Error::other)?;
                }
                InputCommand::Correction {
                    session,
                    context_id,
                    program,
                    before,
                    after,
                    confirmed,
                } => {
                    let origin = self.last_delivered.as_ref().filter(|old| {
                        self.active.is_none()
                            && old.frontend == session
                            && old.target.context_id == context_id
                            && old.program == program
                            && before.len() <= 64 * 1024
                            && after.len() <= 64 * 1024
                            && !before.contains('\0')
                            && !after.contains('\0')
                    });
                    let result = if let Some(origin) = origin {
                        self.engine.correct(
                            origin.key,
                            &json!({"type":"correction",
                            "context_id":context_id,"program":program,"before":before,"after":after,
                            "confirmed":confirmed}),
                            IO_BUDGET,
                        )?
                    } else {
                        CorrectionResult::Rejected
                    };
                    self.input.state(
                        session,
                        match result {
                            CorrectionResult::Changed => "correction_saved",
                            CorrectionResult::Unchanged => "correction_unchanged",
                            CorrectionResult::Rejected => "correction_rejected",
                        },
                    )?;
                }
                _ => {}
            }
        }
        // A burst of Stop/Cancel can already be buffered together. Drain it
        // before a fast final result; overload faults rather than starving a
        // cancellation indefinitely or offering cancelled text for delivery.
        if !drained {
            return Err(invalid("frontend command burst too large"));
        }
        let event = self.engine.poll_event(budget / 2)?;
        // A complete result can arrive after its deadline while blocked in IO.
        // Invalidate it before offering it to the original input context.
        self.enforce_deadline(app)?;
        if let Some((key, event)) = event {
            if let Some(active) = self.active.as_ref().filter(|a| a.key == key) {
                let session = active.frontend;
                let released = matches!(event, ProviderEvent::Released);
                let recording = matches!(event, ProviderEvent::Recording);
                let failed = matches!(event, ProviderEvent::Failed(_));
                let final_text = matches!(event, ProviderEvent::Final(_));
                self.input.last_committed = None;
                app.provider_event(key, event, &mut self.input);
                if final_text && self.input.last_committed.as_ref() == Some(&active.target) {
                    self.last_delivered = Some(RouteSession {
                        frontend: active.frontend,
                        key: active.key,
                        target: active.target.clone(),
                        program: active.program.clone(),
                    });
                }
                if recording {
                    self.input.state(session, "recording")?;
                }
                if failed {
                    self.input.error(session, "辨識失敗，這次沒有送出文字")?;
                }
                if released {
                    self.active = None;
                    self.input.target = None;
                    self.input.state(session, "idle")?;
                }
            }
        }
        Ok(())
    }

    fn enforce_deadline(&mut self, app: &mut Application) -> io::Result<()> {
        if self.active.is_some() && app.expire_dictation(&mut self.engine) {
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok(())
    }

    pub fn suspend(&mut self, budget: Duration) -> io::Result<()> {
        if self.failed {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        self.engine.suspend(budget)
    }

    pub(crate) fn cancel(&mut self, app: &mut Application) {
        self.input.target = None;
        if self.active.is_some() {
            let _ = app.cancel_dictation(&mut self.engine);
        }
    }

    /// Called only by the child owner after wait/reap has proved process exit.
    pub(crate) fn release_after_exit(&mut self, app: &mut Application) {
        self.failed = true;
        self.input.target = None;
        if let Some(active) = self.active.take() {
            app.provider_event(active.key, ProviderEvent::Released, &mut self.input);
        }
    }

    fn matches(&self, session: u64) -> bool {
        self.active.as_ref().is_some_and(|a| a.frontend == session)
    }
}

struct FcitxConnection {
    wire: Wire,
    queued: VecDeque<Value>,
    target: Option<TargetLease>,
    last_committed: Option<TargetLease>,
    faulted: bool,
}

impl FcitxConnection {
    fn accept(peer: UnixStream, budget: Duration) -> io::Result<Self> {
        let mut wire = Wire::authenticated(peer, None)?;
        let nonce = NEXT_HELLO
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| invalid("frontend handshake IDs exhausted"))?;
        let until = deadline(budget);
        wire.send(&json!({"type":"desktop_hello","session":nonce}), until)?;
        if wire.read(until)?
            != json!({"type":"desktop_hello","session":nonce,"value":"voicetype.fcitx.v1"})
        {
            return Err(invalid("incompatible input frontend"));
        }
        Ok(Self {
            wire,
            queued: VecDeque::new(),
            target: None,
            last_committed: None,
            faulted: false,
        })
    }

    fn poll(&mut self, budget: Duration) -> io::Result<Option<InputCommand>> {
        if self.faulted {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let value = match self.queued.pop_front() {
            Some(value) => value,
            None => match self.wire.read(deadline(budget)) {
                Ok(value) => value,
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) =>
                {
                    return Ok(None)
                }
                Err(e) => return Err(e),
            },
        };
        serde_json::from_value(value)
            .map(Some)
            .map_err(|_| invalid("invalid frontend command"))
    }

    fn state(&mut self, session: u64, value: &str) -> io::Result<()> {
        self.wire.send(
            &json!({"type":"state","session":session,"value":value}),
            deadline(IO_BUDGET),
        )
    }

    fn error(&mut self, session: u64, text: &str) -> io::Result<()> {
        self.wire.send(
            &json!({"type":"error","session":session,"code":"internal","text":text}),
            deadline(IO_BUDGET),
        )
    }
}

impl DeliveryPort for FcitxConnection {
    fn commit_if_focused(&mut self, target: &TargetLease, text: &str) -> DeliveryOutcome {
        if self.faulted {
            return DeliveryOutcome::Unconfirmed;
        }
        if self.target.as_ref() != Some(target) {
            return DeliveryOutcome::FocusChanged;
        }
        self.target = None;
        let until = deadline(IO_BUDGET);
        let result = (|| {
            self.wire.send(
                &json!({"type":"deliver","session":target.generation,
                "context_id":target.context_id,"text":text}),
                until,
            )?;
            loop {
                let reply = self.wire.read(until)?;
                if reply["type"] == "delivered" {
                    if reply["session"] != target.generation
                        || reply["context_id"] != target.context_id
                    {
                        return Err(invalid("unmatched delivery acknowledgement"));
                    }
                    return match reply["code"].as_str() {
                        Some("committed") => Ok(DeliveryOutcome::Delivered),
                        Some("stale" | "focus_changed") => Ok(DeliveryOutcome::FocusChanged),
                        _ => Err(invalid("invalid delivery acknowledgement")),
                    };
                }
                if self.queued.len() == 32 {
                    return Err(invalid("frontend command queue full"));
                }
                self.queued.push_back(reply);
            }
        })();
        match result {
            Ok(outcome) => {
                if outcome == DeliveryOutcome::Delivered {
                    self.last_committed = Some(target.clone());
                }
                outcome
            }
            Err(_) => {
                self.faulted = true;
                // The commit may already have happened. Retain text visibly,
                // never retry it or pretend it was definitely not inserted.
                DeliveryOutcome::Unconfirmed
            }
        }
    }
}
