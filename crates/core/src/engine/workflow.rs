//! Guided tasks, preflight, reusable non-secret profiles and reports.
use super::*;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WorkflowGoal {
    #[default]
    Transfer,
    Upgrade,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkflowTarget {
    pub device: DeviceId,
    pub local: String,
    pub remote: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowSpec {
    pub goal: WorkflowGoal,
    pub receive: bool,
    pub protocol: Protocol,
    pub targets: Vec<WorkflowTarget>,
}
impl Default for WorkflowSpec {
    fn default() -> Self {
        Self {
            goal: WorkflowGoal::Transfer,
            receive: false,
            protocol: Protocol::Sftp,
            targets: Vec::new(),
        }
    }
}
#[derive(Debug, Clone)]
pub struct Preflight {
    pub device: DeviceId,
    pub endpoint: Option<CopyEndpoint>,
    pub command: Option<String>,
    pub blocker: Option<String>,
    pub warnings: Vec<String>,
}
#[derive(Debug, Clone)]
pub struct WorkflowSnapshot {
    pub id: u64,
    pub spec: WorkflowSpec,
    pub root: PathBuf,
    pub jobs: Vec<OperationId>,
    pub old_versions: HashMap<DeviceId, String>,
    pub verified_hashes: HashMap<DeviceId, String>,
    pub started: chrono::DateTime<chrono::Utc>,
    pub finished: Option<chrono::DateTime<chrono::Utc>>,
    pub owned_services: Vec<ServiceId>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkProfile {
    pub name: String,
    pub goal: WorkflowGoal,
    pub receive: bool,
    pub protocol: String,
    pub advertise: Option<String>,
    pub targets: Vec<ProfileTarget>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfileTarget {
    pub host: String,
    pub port: u16,
    pub local: String,
    pub remote: String,
}
#[derive(Default, Serialize, Deserialize)]
struct Profiles {
    #[serde(default)]
    profiles: Vec<WorkProfile>,
}
#[derive(Debug, Clone)]
pub struct QueueReview {
    pub operation: OperationId,
    pub device: DeviceId,
    pub revision: u64,
    pub ready: bool,
    pub message: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferInspection {
    Verified,
    Missing,
    Partial { actual: u64, expected: u64 },
    Unknown(String),
}
impl TransferInspection {
    pub fn label(&self) -> String {
        match self {
            Self::Verified => "Destination hash matches · no copy needed".into(),
            Self::Missing => "Destination is absent · retry available".into(),
            Self::Partial { actual, expected } => {
                format!("Destination differs ({actual}/{expected} bytes) · confirm overwrite")
            }
            Self::Unknown(reason) => format!("Destination status unknown: {reason}"),
        }
    }
}
impl Engine {
    pub fn preflight(&self, spec: &WorkflowSpec) -> Vec<Preflight> {
        let mut results = Vec::with_capacity(spec.targets.len());
        for target in &spec.targets {
            let mut result = Preflight {
                device: target.device,
                endpoint: None,
                command: None,
                blocker: None,
                warnings: Vec::new(),
            };
            let check = (|| -> Result<(), String> {
                let app = self.app()?;
                let cfg = app.config.read().unwrap().clone();
                let sw = self.device(target.device)?;
                if sw.state() != SwitchState::Ready {
                    return Err("Connect the device and wait until it is ready".into());
                }
                if self.busy(target.device) {
                    return Err("Finish or cancel existing jobs for this device first".into());
                }
                if spec.goal == WorkflowGoal::Upgrade && spec.receive {
                    return Err("Upgrade images must be local".into());
                }
                let protocol = if spec.receive {
                    Protocol::Ftp
                } else {
                    spec.protocol
                };
                if spec.goal == WorkflowGoal::Transfer {
                    let (job, _) = self.transfer_job(
                        &sw,
                        &target.local,
                        &target.remote,
                        spec.receive,
                        protocol,
                    )?;
                    if let Job::Copy { command, .. } = job {
                        result.command = Some(command);
                    }
                } else {
                    let abs = fsroot::SecureRoot::new(&cfg.root)
                        .map_err(|e| e.to_string())?
                        .resolve(&target.local)
                        .map_err(|e| e.to_string())?;
                    if !abs.is_file() {
                        return Err("Select an upgrade image".into());
                    }
                    if upgrade::image_version(&target.local).is_none() {
                        return Err("Select a complete release .bin image".into());
                    }
                    let size = abs.metadata().map_err(|e| e.to_string())?.len();
                    let facts = sw.facts();
                    if facts
                        .flash
                        .is_some_and(|f| f.free < size.saturating_add(64 * 1024 * 1024))
                    {
                        result.warnings.push("Free flash may be insufficient; inspect existing images or Remove inactive".into());
                    }
                    if let Some(info) = facts.version.as_ref() {
                        if info.version.as_ref().is_some_and(|v| {
                            Some(upgrade::normalize_version(v))
                                == upgrade::image_version(&target.local)
                        }) {
                            result.warnings.push("Target release already installed; copying is possible, installation will be blocked".into());
                        }
                    }
                    let endpoint = copy_endpoint(&cfg, protocol, sw.transfer_peer().as_ref())?;
                    let remote = format!(
                        "{}{}",
                        facts.flash_device,
                        target.local.rsplit('/').next().unwrap_or(&target.local)
                    );
                    result.command = Some(cisco::deploy_command(
                        protocol,
                        &cfg,
                        &endpoint.ip,
                        &target.local,
                        &remote,
                    )?);
                }
                result.endpoint = Some(copy_endpoint(&cfg, protocol, sw.transfer_peer().as_ref())?);
                if let Some(warning) = sw
                    .facts()
                    .version
                    .as_ref()
                    .and_then(|v| cisco::platform_warning(v, &target.local))
                {
                    result.warnings.push(warning);
                }
                if !app
                    .services
                    .status(cisco::service_of(protocol))
                    .is_running()
                {
                    result.warnings.push(format!(
                        "{} service will start for this task",
                        protocol.label()
                    ));
                }
                Ok(())
            })();
            if let Err(error) = check {
                result.blocker = Some(error);
            }
            results.push(result);
        }
        results
    }
    pub(super) fn save_workflow(
        &self,
        id: u64,
        previous: Option<u64>,
        spec: WorkflowSpec,
    ) -> Result<u64, String> {
        if spec.targets.len() > 1000 {
            return Err("Use at most 1000 targets/files".into());
        }
        for target in &spec.targets {
            self.device(target.device)?;
        }
        let root = self.app()?.config.read().unwrap().root.clone();
        let mut state = self.state.lock().unwrap();
        if let Some(previous) = previous {
            let workflow = state
                .workflows
                .iter_mut()
                .find(|w| w.id == previous)
                .ok_or("workflow no longer exists")?;
            if !workflow.jobs.is_empty() {
                return Err("An executed workflow cannot be edited; create a new workflow".into());
            }
            workflow.spec = spec;
            workflow.root = root;
            state.workflow_revision += 1;
            return Ok(previous);
        }
        if state.workflows.len() >= 100 {
            return Err("At most 100 workflows per session".into());
        }
        state.workflow_revision += 1;
        state.workflows.push(WorkflowSnapshot {
            id,
            spec,
            root,
            jobs: Vec::new(),
            old_versions: HashMap::new(),
            verified_hashes: HashMap::new(),
            started: chrono::Utc::now(),
            finished: None,
            owned_services: Vec::new(),
        });
        Ok(id)
    }
    pub(super) fn start_workflow(self: &Arc<Self>, id: u64) -> Result<(), String> {
        let spec = {
            let state = self.state.lock().unwrap();
            let workflow = state
                .workflows
                .iter()
                .find(|w| w.id == id)
                .ok_or("workflow no longer exists")?;
            if !workflow.jobs.is_empty() {
                return Err("Workflow already started; inspect or retry its jobs".into());
            }
            if self.app()?.config.read().unwrap().root != workflow.root {
                return Err("Local root changed; create a new workflow".into());
            }
            workflow.spec.clone()
        };
        if spec.targets.is_empty() {
            return Err("Select devices and files first".into());
        }
        if spec.goal == WorkflowGoal::Upgrade {
            let mut seen = std::collections::HashSet::new();
            if spec.targets.iter().any(|t| !seen.insert(t.device)) {
                return Err("Assign exactly one image per upgrade device".into());
            }
        }
        let checks = self.preflight(&spec);
        if checks.iter().any(|p| p.blocker.is_some()) {
            return Err("Resolve the preflight blockers before starting".into());
        }
        let app = self.app()?;
        let protocol = if spec.receive {
            Protocol::Ftp
        } else {
            spec.protocol
        };
        let service = cisco::service_of(protocol);
        let owned = matches!(
            app.services.status(service),
            ServiceStatus::Stopped | ServiceStatus::Failed(_)
        );
        let old_versions = spec
            .targets
            .iter()
            .filter_map(|t| {
                self.device(t.device).ok().map(|sw| {
                    (
                        t.device,
                        sw.facts()
                            .version
                            .and_then(|v| v.version)
                            .unwrap_or_else(|| "unknown".into()),
                    )
                })
            })
            .collect();
        let before: std::collections::HashSet<_> = self
            .state
            .lock()
            .unwrap()
            .operations
            .iter()
            .map(|o| o.id)
            .collect();
        if spec.goal == WorkflowGoal::Transfer {
            let requests = spec
                .targets
                .iter()
                .map(|t| TransferRequest {
                    device: t.device,
                    local: t.local.clone(),
                    remote: t.remote.clone(),
                    receive: spec.receive,
                    protocol,
                    overwrite: false,
                    platform_check: true,
                })
                .collect();
            let first = self.next.fetch_add(1, Ordering::Relaxed);
            self.queue_transfers(first, requests)?;
        } else {
            // All assignments are validated before the first job is created.
            for target in &spec.targets {
                self.assign_image(vec![target.device], target.local.clone())?;
            }
            for target in &spec.targets {
                let operation = self.next.fetch_add(1, Ordering::Relaxed);
                self.device(target.device)?.choose_protocol(protocol);
                self.deploy_image(operation, target.device, protocol, false)?;
            }
        }
        let mut state = self.state.lock().unwrap();
        let jobs = state
            .operations
            .iter()
            .filter(|o| !before.contains(&o.id))
            .map(|o| o.id)
            .collect();
        let workflow = state.workflows.iter_mut().find(|w| w.id == id).unwrap();
        workflow.jobs = jobs;
        workflow.old_versions = old_versions;
        workflow.started = chrono::Utc::now();
        if owned {
            workflow.owned_services.push(service);
        }
        state.workflow_revision += 1;
        Ok(())
    }
    pub(super) fn finish_workflows(&self) {
        let Ok(app) = self.app() else { return };
        let progress: HashMap<_, _> = app
            .switches
            .list()
            .iter()
            .map(|sw| (sw.id, sw.upgrade()))
            .collect();
        let mut state = self.state.lock().unwrap();
        let terminal: std::collections::HashSet<_> = state
            .operations
            .iter()
            .filter(|o| !o.state.is_active() && !matches!(o.state, OperationState::Uncertain(_)))
            .map(|o| o.id)
            .collect();
        let assignments = state.assignments.clone();
        let mut completed = Vec::new();
        let mut changed = false;
        for workflow in &mut state.workflows {
            for target in &workflow.spec.targets {
                if assignments
                    .get(&target.device)
                    .is_none_or(|(local, _)| local != &target.local)
                {
                    continue;
                }
                if let Some(upgrade::Progress::Verified { md5, .. }) = progress.get(&target.device)
                {
                    if workflow.verified_hashes.get(&target.device) != Some(md5) {
                        workflow.verified_hashes.insert(target.device, md5.clone());
                        changed = true;
                    }
                }
            }
            let uploads_finished =
                !workflow.jobs.is_empty() && workflow.jobs.iter().all(|id| terminal.contains(id));
            let upgrades_finished = workflow.spec.goal == WorkflowGoal::Transfer
                || workflow.spec.targets.iter().all(|t| {
                    matches!(
                        progress.get(&t.device),
                        Some(upgrade::Progress::Complete { .. } | upgrade::Progress::Failed(_))
                    )
                });
            if uploads_finished && upgrades_finished && workflow.finished.is_none() {
                workflow.finished = Some(chrono::Utc::now());
                changed = true;
                completed.push(workflow.clone());
            }
        }
        if changed {
            state.workflow_revision += 1;
        }
        drop(state);
        for workflow in completed {
            if let Ok(report) = self.build_workflow_report(&workflow) {
                let mut state = self.state.lock().unwrap();
                if state.workflows.iter().any(|w| {
                    w.id == workflow.id
                        && w.finished == workflow.finished
                        && w.jobs == workflow.jobs
                }) {
                    state.workflow_reports.insert(workflow.id, report);
                }
            }
        }
    }
    pub(super) fn stop_workflow_services(&self, id: u64) -> Result<(), String> {
        let app = self.app()?;
        let state = self.state.lock().unwrap();
        let workflow = state
            .workflows
            .iter()
            .find(|w| w.id == id)
            .ok_or("workflow no longer exists")?;
        for service in &workflow.owned_services {
            if state.operations.iter().any(|o| {
                o.state.is_active()
                    && o.transfer
                        .as_ref()
                        .is_some_and(|t| cisco::service_of(t.protocol) == *service)
            }) || app
                .sessions
                .snapshot()
                .iter()
                .any(|t| t.state.is_active() && cisco::service_of(t.protocol) == *service)
            {
                return Err("Service is still in use by another job or session".into());
            }
        }
        for service in &workflow.owned_services {
            app.services.stop(*service);
        }
        Ok(())
    }
    pub(super) fn save_profile(&self, name: String, workflow: u64) -> Result<(), String> {
        if name.trim().is_empty() {
            return Err("Enter a profile name".into());
        }
        let app = self.app()?;
        let cfg = app.config.read().unwrap().clone();
        let spec = self
            .state
            .lock()
            .unwrap()
            .workflows
            .iter()
            .find(|w| w.id == workflow)
            .ok_or("workflow no longer exists")?
            .spec
            .clone();
        let targets = spec
            .targets
            .iter()
            .map(|t| {
                self.device(t.device).map(|sw| ProfileTarget {
                    host: sw.host.clone(),
                    port: sw.port,
                    local: t.local.clone(),
                    remote: t.remote.clone(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let profile = WorkProfile {
            name: name.trim().into(),
            goal: spec.goal,
            receive: spec.receive,
            protocol: spec.protocol.label().into(),
            advertise: cfg.advertise,
            targets,
        };
        let mut profiles = self.state.lock().unwrap().profiles.clone();
        if let Some(old) = profiles.iter_mut().find(|p| p.name == profile.name) {
            *old = profile;
        } else {
            profiles.push(profile);
        }
        let content = toml::to_string_pretty(&Profiles {
            profiles: profiles.clone(),
        })
        .map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&cfg.config_dir).map_err(|e| e.to_string())?;
        let temp = cfg.config_dir.join("profiles.toml.tmp");
        std::fs::write(&temp, content).map_err(|e| e.to_string())?;
        std::fs::rename(temp, cfg.config_dir.join("profiles.toml")).map_err(|e| e.to_string())?;
        let mut state = self.state.lock().unwrap();
        state.profiles = profiles;
        state.workflow_revision += 1;
        Ok(())
    }
    pub(super) fn load_profiles(&self, app: &App) {
        let path = app.config.read().unwrap().config_dir.join("profiles.toml");
        match std::fs::read_to_string(path) {
            Ok(content) => match toml::from_str::<Profiles>(&content) {
                Ok(profiles) => self.state.lock().unwrap().profiles = profiles.profiles,
                Err(error) => app.logger.log(
                    logging::Event::new(
                        logging::LogLevel::Warning,
                        "core",
                        "Could not load work profiles",
                    )
                    .result(error.to_string()),
                ),
            },
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => app.logger.log(
                logging::Event::new(
                    logging::LogLevel::Warning,
                    "core",
                    "Could not read work profiles",
                )
                .result(error.to_string()),
            ),
            _ => {}
        }
    }
    pub(super) fn export_workflow(&self, id: u64, local: String) -> Result<(), String> {
        let app = self.app()?;
        let (workflow, report) = {
            let state = self.state.lock().unwrap();
            (
                state
                    .workflows
                    .iter()
                    .find(|w| w.id == id)
                    .ok_or("workflow no longer exists")?
                    .clone(),
                state.workflow_reports.get(&id).cloned(),
            )
        };
        let report = report
            .map(Ok)
            .unwrap_or_else(|| self.build_workflow_report(&workflow))?;
        let cfg = app.config.read().unwrap().clone();
        let path = fsroot::SecureRoot::new(&cfg.root)
            .map_err(|e| e.to_string())?
            .resolve_for_write(&local)
            .map_err(|e| e.to_string())?;
        if path.exists() {
            return Err("Report file already exists; choose a new name".into());
        }
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| e.to_string())?;
        file.write_all(report.as_bytes())
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    fn build_workflow_report(&self, workflow: &WorkflowSnapshot) -> Result<String, String> {
        let operations = self.state.lock().unwrap().operations.clone();
        fn csv(text: &str) -> String {
            format!("\"{}\"", text.replace('"', "\"\""))
        }
        let mut report =
            "hostname,ip,model,old_release,new_release,md5,status,workflow_elapsed_seconds,error,local_file,remote_file,protocol\n"
                .to_string();
        let elapsed =
            (workflow.finished.unwrap_or_else(chrono::Utc::now) - workflow.started).num_seconds();
        for target in &workflow.spec.targets {
            let sw = self.device(target.device)?;
            let facts = sw.facts();
            let version = facts.version.unwrap_or_default();
            let status = sw.upgrade();
            let hash = workflow
                .verified_hashes
                .get(&sw.id)
                .map(String::as_str)
                .unwrap_or("");
            let operation = workflow
                .jobs
                .iter()
                .filter_map(|id| {
                    operations.iter().find(|o| {
                        o.id == *id
                            && o.device == Some(target.device)
                            && (workflow.spec.goal == WorkflowGoal::Upgrade
                                || o.transfer.as_ref().is_some_and(|t| {
                                    t.local == target.local && t.remote == target.remote
                                }))
                    })
                })
                .next_back();
            let label = if workflow.spec.goal == WorkflowGoal::Upgrade {
                status.label()
            } else {
                operation
                    .map(|o| o.state.label().into())
                    .unwrap_or_else(|| "Draft".into())
            };
            let error = match &status {
                upgrade::Progress::Failed(e) => e.as_str(),
                _ => operation.and_then(|o| o.state.error()).unwrap_or(""),
            };
            let cells = [
                sw.display_name(),
                sw.host.clone(),
                version.model.unwrap_or_default(),
                workflow
                    .old_versions
                    .get(&sw.id)
                    .cloned()
                    .unwrap_or_default(),
                version.version.unwrap_or_default(),
                hash.into(),
                label,
                elapsed.to_string(),
                error.into(),
                target.local.clone(),
                target.remote.clone(),
                workflow.spec.protocol.label().into(),
            ];
            report.push_str(&cells.iter().map(|s| csv(s)).collect::<Vec<_>>().join(","));
            report.push('\n');
        }
        Ok(report)
    }
}
