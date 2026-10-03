//! Serialize submissions off the frame thread, including config and filesystem checks.
use super::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

pub(super) enum Context {
    None,
    Root(PathBuf),
    Connect(ConnectionForm),
    Service(ServiceForm),
    Cli(u64),
    Retry(u64),
    Workflow { generation: u64, start: bool },
    WorkflowStart { generation: u64 },
    Credentials(u64, ConnectionForm),
}
pub(crate) struct Outcome {
    pub(super) result: Result<u64, String>,
    pub(super) context: Context,
    pub(super) quiet: bool,
    pub(super) queued: bool,
}
struct Submission {
    app: SharedApp,
    command: Command,
    context: Context,
    quiet: bool,
    queued: bool,
    tone: Option<transferbuddy_core::sound::Tone>,
}
pub(super) struct Worker {
    tx: mpsc::Sender<Submission>,
    stopped: Arc<AtomicBool>,
}
impl Worker {
    pub fn new(ctx: egui::Context, events: mpsc::Sender<UiEvent>) -> Self {
        let (tx, rx) = mpsc::channel::<Submission>();
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        std::thread::spawn(move || {
            while let Ok(job) = rx.recv() {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                let result = job.app.engine().submit(job.command);
                if let Some(tone) = job.tone {
                    let sound = job.app.config.read().unwrap().sound;
                    transferbuddy_core::sound::play(
                        sound,
                        if result.is_ok() {
                            tone
                        } else {
                            transferbuddy_core::sound::Tone::Error
                        },
                    );
                }
                let _ = events.send(UiEvent::Submitted(Outcome {
                    result,
                    context: job.context,
                    quiet: job.quiet,
                    queued: job.queued,
                }));
                ctx.request_repaint();
            }
        });
        Self { tx, stopped }
    }
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}
impl Desktop {
    pub fn send(&mut self, command: Command) -> bool {
        let Some(app) = &self.app else { return false };
        let context = match &command {
            Command::SaveWorkflow { .. } => Context::Workflow {
                generation: self.wizard.generation,
                start: self.wizard.start_after_save,
            },
            Command::StartWorkflow(id) if self.wizard.workflow == Some(*id) => {
                Context::WorkflowStart {
                    generation: self.wizard.generation,
                }
            }
            Command::UpdateCredentials(id, c) => Context::Credentials(
                *id,
                ConnectionForm {
                    user: c.username.clone(),
                    password: c.password.clone(),
                    enable: c.enable_password.clone(),
                    ..Default::default()
                },
            ),
            Command::Set(Setting::Advertise(_)) if self.retry_saving.is_some() => {
                Context::Retry(self.retry_saving.unwrap())
            }
            Command::ChangeRoot(root) => Context::Root(root.clone()),
            Command::OpenCli(id) => Context::Cli(*id),
            Command::Connect {
                host,
                port,
                credentials,
                bulk,
            } => Context::Connect(ConnectionForm {
                targets: host.clone(),
                port: port.to_string(),
                user: credentials.username.clone(),
                password: credentials.password.clone(),
                enable: credentials.enable_password.clone(),
                bulk: *bulk,
                error: String::new(),
            }),
            Command::BulkConnect {
                targets,
                port,
                credentials,
            } => Context::Connect(ConnectionForm {
                targets: targets.clone(),
                port: port.to_string(),
                user: credentials.username.clone(),
                password: credentials.password.clone(),
                enable: credentials.enable_password.clone(),
                bulk: true,
                error: String::new(),
            }),
            Command::SetMany(_) => self
                .pending_service
                .take()
                .map(Context::Service)
                .unwrap_or(Context::None),
            _ => Context::None,
        };
        let quiet = matches!(
            &command,
            Command::ResizeCli(..)
                | Command::CliInput(..)
                | Command::ListRemote(..)
                | Command::ListLocal(..)
        );
        let queued = matches!(
            &command,
            Command::QueueTransfers(_)
                | Command::Transfer { .. }
                | Command::Deploy(..)
                | Command::Verify(..)
                | Command::RetryOperation(_)
        );
        let tone = match &command {
            Command::StartService(_) | Command::StartEnabled => {
                Some(transferbuddy_core::sound::Tone::On)
            }
            Command::StopService(_) | Command::StopAll => {
                Some(transferbuddy_core::sound::Tone::Off)
            }
            Command::Set(_)
            | Command::SetMany(_)
            | Command::ChooseProtocol(..)
            | Command::Connect { .. }
            | Command::BulkConnect { .. } => Some(transferbuddy_core::sound::Tone::Confirm),
            _ => None,
        };
        // The picker queues its following copy before the worker acknowledges the choice.
        if let Command::ChooseProtocol(id, protocol) = &command {
            if let Some(device) = self.snapshot.devices.iter_mut().find(|d| d.id == *id) {
                device.protocol = *protocol;
            }
        }
        let sent = self
            .submissions
            .tx
            .send(Submission {
                app: app.clone(),
                command,
                context,
                quiet,
                queued,
                tone,
            })
            .is_ok();
        if sent {
            self.pending_submissions += 1;
            if !quiet {
                self.status = "Submitting…".into();
            }
        } else {
            self.status = "Command worker unavailable".into();
        }
        sent
    }
    pub(super) fn submitted(&mut self, outcome: Outcome) {
        self.pending_submissions = self.pending_submissions.saturating_sub(1);
        if let Context::Cli(id) = &outcome.context {
            if let Some(window) = self.console_windows.get_mut(id) {
                window.opened(outcome.result.is_ok());
            }
            if outcome.result.is_err() && self.cli == Some(*id) {
                self.cli = None;
                self.cli_focus = false;
            }
        }
        if let Context::Credentials(id, _) = &outcome.context {
            if outcome.result.is_ok() {
                self.send(Command::Reconnect(*id));
            }
        }
        if let Context::Workflow { generation, start } = &outcome.context {
            if *generation != self.wizard.generation {
                return;
            }
            self.wizard.start_after_save = false;
            if let Ok(id) = outcome.result.as_ref() {
                self.wizard.workflow = Some(*id);
                if *start {
                    self.wizard.start_after_save = true;
                    if self.send(Command::StartWorkflow(*id)) {
                        self.wizard.step = 4;
                    } else {
                        self.wizard.start_after_save = false;
                    }
                }
            }
        }
        if let Context::WorkflowStart { generation } = &outcome.context {
            if *generation != self.wizard.generation {
                return;
            }
            self.wizard.start_after_save = false;
            if outcome.result.is_err() {
                self.wizard.step = 3;
            }
        }
        if let Context::Retry(id) = &outcome.context {
            self.retry_saving = None;
            if outcome.result.is_ok() {
                self.retry_interface = None;
                self.send(Command::RetryOperation(*id));
            }
        }
        match outcome.result {
            Ok(id) => {
                if !outcome.quiet {
                    self.status = if outcome.queued {
                        format!("Job #{id} queued")
                    } else {
                        "Done".into()
                    };
                }
                if outcome.queued {
                    self.job_selected = Some(id);
                    self.jobs_open = true;
                }
                if let Context::Root(root) = outcome.context {
                    self.prefs.root = Some(root);
                    self.wizard = super::wizard::Wizard::default();
                    self.local_dir.clear();
                    self.remote_dir.clear();
                    self.remote_loaded = None;
                    self.local_selection = None;
                    self.remote_selection = None;
                    self.local_selected.clear();
                    self.remote_selected.clear();
                    self.device_workspaces.clear();
                    self.pending_transfers = None;
                    self.protocol_picker = None;
                    self.protocol_index = None;
                    self.protocol_shown = None;
                    self.image = None;
                    self.refresh_local();
                }
            }
            Err(error) => {
                self.status = error.clone();
                match outcome.context {
                    Context::Connect(mut form) => {
                        form.error = error;
                        self.connect = Some(form);
                    }
                    Context::Credentials(id, mut form) => {
                        if let Some(d) = self.snapshot.devices.iter().find(|d| d.id == id) {
                            form.targets = d.host.clone();
                            form.port = d.port.to_string();
                        }
                        form.error = error;
                        self.credentials_device = Some(id);
                        self.connect = Some(form);
                    }
                    Context::Service(form) => self.service = Some(form),
                    _ => {}
                }
                self.remote_loaded = None;
            }
        }
    }
    pub(super) fn copy_commands(&mut self, path: String, ctx: &egui::Context) {
        let Some(app) = self.app.clone() else { return };
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        self.runtime.spawn_blocking(move || {
            let _ = tx.send(UiEvent::CopyCommands(engine::copy_commands(&app, &path)));
            ctx.request_repaint();
        });
    }
}
