//! Session-scoped FIFO queues. The frozen request, rather than frontend selection,
//! owns every path and protocol. Filesystem checks run on the runtime's worker pool.
use super::*;
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;

#[derive(Debug, Clone)]
pub struct TransferRequest {
    pub device: DeviceId,
    pub local: String,
    pub remote: String,
    pub receive: bool,
    pub protocol: Protocol,
    pub overwrite: bool,
    pub platform_check: bool,
}
#[derive(Debug, Clone)]
pub struct TransferDetails {
    pub command: Option<String>,
    pub inspection: Option<workflow::TransferInspection>,
    pub local: String,
    pub remote: String,
    pub receive: bool,
    pub protocol: Protocol,
    pub size: u64,
    pub bytes: u64,
    pub speed: f64,
    pub eta: Option<Duration>,
    pub cancellable: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    size: u64,
    modified: Option<SystemTime>,
}
#[derive(Clone)]
enum Work {
    Transfer(TransferRequest),
    Image(Job),
}
#[derive(Clone)]
struct Entry {
    device: DeviceId,
    retry_origin: Option<OperationId>,
    root: PathBuf,
    work: Work,
    protocol: Option<Protocol>,
    stamp: Option<Stamp>,
    prepared: bool,
    cancel: Arc<AtomicBool>,
}
#[derive(Default)]
pub(super) struct Queue {
    order: VecDeque<OperationId>,
    entries: HashMap<OperationId, Entry>,
    paused: HashSet<DeviceId>,
}
impl Queue {
    pub(super) fn pause(&mut self, device: DeviceId) {
        self.paused.insert(device);
    }
    pub(super) fn forget_device(&mut self, device: DeviceId) {
        self.paused.remove(&device);
    }
    pub(super) fn entries_remove(&mut self, id: OperationId) {
        self.entries.remove(&id);
    }
    #[cfg(test)]
    pub(super) fn overwrite_for_test(&self, id: OperationId) -> bool {
        self.entries.get(&id).is_some_and(|e| {
            matches!(&e.work, Work::Transfer(r) if r.overwrite)
                || matches!(
                    &e.work,
                    Work::Image(Job::PrepareUpgrade {
                        overwrite: true,
                        ..
                    })
                )
        })
    }
    #[cfg(test)]
    pub(super) fn prepared_for_test(&self, id: OperationId) -> bool {
        self.entries.get(&id).is_some_and(|e| e.prepared)
    }
}

impl Engine {
    pub(super) fn queue_transfers(
        self: &Arc<Self>,
        first: OperationId,
        requests: Vec<TransferRequest>,
    ) -> Result<(), String> {
        if requests.is_empty() {
            return Err("select at least one file".into());
        }
        if requests.len() > 1000 {
            return Err("select at most 1000 files per batch".into());
        }
        // Validate the whole batch before creating any jobs. No filesystem I/O here.
        for request in &requests {
            self.device(request.device)?;
            deploy::check_storage_path(&request.remote)?;
            if request.receive && request.protocol != Protocol::Ftp {
                return Err("downloads use FTP".into());
            }
        }
        let root = self.app()?.config.read().unwrap().root.clone();
        for (index, request) in requests.into_iter().enumerate() {
            let id = if index == 0 {
                first
            } else {
                self.next.fetch_add(1, Ordering::Relaxed)
            };
            let details = TransferDetails {
                command: None,
                inspection: None,
                local: request.local.clone(),
                remote: request.remote.clone(),
                receive: request.receive,
                protocol: request.protocol,
                size: 0,
                bytes: 0,
                speed: 0.0,
                eta: None,
                cancellable: true,
            };
            let entry = Entry {
                device: request.device,
                retry_origin: None,
                root: root.clone(),
                protocol: Some(request.protocol),
                work: Work::Transfer(request),
                stamp: None,
                prepared: false,
                cancel: Arc::new(AtomicBool::new(false)),
            };
            self.add_queued(id, entry, details, "copy file");
        }
        Ok(())
    }
    pub(super) fn queue_image(
        self: &Arc<Self>,
        id: OperationId,
        sw: Arc<Switch>,
        job: Job,
        protocol: Option<Protocol>,
        label: &str,
    ) {
        let (local, remote, size, p) = match &job {
            Job::PrepareUpgrade {
                rel_path,
                remote,
                size,
                protocol,
                ..
            } => (rel_path.clone(), remote.clone(), *size, *protocol),
            Job::VerifyUpgrade {
                rel_path, remote, ..
            } => (rel_path.clone(), remote.clone(), 0, sw.protocol()),
            _ => unreachable!(),
        };
        let root = self.app().unwrap().config.read().unwrap().root.clone();
        let details = TransferDetails {
            command: None,
            inspection: None,
            local,
            remote,
            receive: false,
            protocol: p,
            size,
            bytes: 0,
            speed: 0.0,
            eta: None,
            cancellable: protocol.is_some(),
        };
        self.add_queued(
            id,
            Entry {
                device: sw.id,
                retry_origin: None,
                root,
                work: Work::Image(job),
                protocol,
                stamp: None,
                prepared: false,
                cancel: Arc::new(AtomicBool::new(false)),
            },
            details,
            label,
        );
    }
    fn add_queued(
        self: &Arc<Self>,
        id: OperationId,
        entry: Entry,
        details: TransferDetails,
        label: &str,
    ) {
        self.new_operation(id, Some(entry.device), label);
        {
            let mut state = self.state.lock().unwrap();
            let op = state.operations.iter_mut().find(|o| o.id == id).unwrap();
            op.state = OperationState::Queued;
            op.transfer = Some(details);
            state.queue.order.push_back(id);
            state.queue.entries.insert(id, entry.clone());
        }
        let engine = self.clone();
        let runtime = self.app().unwrap().runtime.clone();
        runtime.spawn(async move {
            let e = engine.clone();
            let checked = tokio::task::spawn_blocking(move || e.check_entry(&entry))
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r);
            let mut state = engine.state.lock().unwrap();
            let cancelled = state
                .queue
                .entries
                .get(&id)
                .is_none_or(|e| e.cancel.load(Ordering::Acquire));
            if cancelled {
                return;
            }
            match checked {
                Ok(stamp) => {
                    let entry = state.queue.entries.get_mut(&id).unwrap();
                    if entry.stamp.as_ref().is_some_and(|old| *old != stamp) {
                        state.queue.order.retain(|key| *key != id);
                        drop(state);
                        engine.finish(
                            id,
                            Err(
                                "source file changed since the original job; select it again"
                                    .into(),
                            ),
                        );
                        return;
                    }
                    entry.prepared = true;
                    entry.stamp = Some(stamp.clone());
                    if let Some(details) = state
                        .operations
                        .iter_mut()
                        .find(|o| o.id == id)
                        .and_then(|o| o.transfer.as_mut())
                    {
                        details.size = stamp.size;
                    }
                }
                Err(error) => {
                    state.queue.order.retain(|key| *key != id);
                    drop(state);
                    engine.finish(id, Err(error));
                }
            }
            let _ = engine.events.send(AppEvent::Changed);
        });
    }
    fn check_entry(&self, entry: &Entry) -> Result<Stamp, String> {
        let app = self.app()?;
        if app.config.read().unwrap().root != entry.root {
            return Err("local root changed; create a new job".into());
        }
        let sw = self.device(entry.device)?;
        let local = match &entry.work {
            Work::Transfer(r) => {
                let (_, size) =
                    self.transfer_job(&sw, &r.local, &r.remote, r.receive, r.protocol)?;
                if r.receive {
                    return Ok(Stamp {
                        size,
                        modified: None,
                    });
                }
                &r.local
            }
            Work::Image(
                Job::PrepareUpgrade { rel_path, .. } | Job::VerifyUpgrade { rel_path, .. },
            ) => rel_path,
            _ => unreachable!(),
        };
        let path = fsroot::SecureRoot::new(&entry.root)
            .map_err(|e| e.to_string())?
            .resolve(local)
            .map_err(|e| e.to_string())?;
        let metadata = path.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file() {
            return Err("source is not a file".into());
        }
        Ok(Stamp {
            size: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }
    /// Called by the shared engine task, never by a frontend frame.
    pub(super) fn dispatch_queue(self: &Arc<Self>) {
        let ids = self
            .state
            .lock()
            .unwrap()
            .queue
            .order
            .iter()
            .copied()
            .collect::<Vec<_>>();
        let mut seen = HashSet::new();
        for id in ids {
            let mut state = self.state.lock().unwrap();
            let Some(entry) = state.queue.entries.get(&id).cloned() else {
                continue;
            };
            // The order snapshot can outlive a cancellation or failed preparation.
            if entry.cancel.load(Ordering::Acquire) || !state.queue.order.contains(&id) {
                continue;
            }
            if !seen.insert(entry.device) {
                continue;
            }
            let Ok(sw) = self.device(entry.device) else {
                state.queue.order.retain(|key| *key != id);
                drop(state);
                self.finish(id, Err("device no longer exists".into()));
                continue;
            };
            if sw.state().is_over() {
                state.queue.paused.insert(entry.device);
            }
            if state.queue.paused.contains(&entry.device) {
                if let Some(op) = state.operations.iter_mut().find(|o| o.id == id) {
                    op.state = OperationState::Paused;
                }
                continue;
            }
            if !entry.prepared
                || sw.state() != SwitchState::Ready
                || state.operations.iter().any(|o| {
                    o.id != id
                        && o.device == Some(entry.device)
                        && matches!(
                            o.state,
                            OperationState::Running
                                | OperationState::Preparing
                                | OperationState::Stopping
                        )
                })
            {
                continue;
            }
            state.queue.order.retain(|key| *key != id);
            state
                .operations
                .iter_mut()
                .find(|o| o.id == id)
                .unwrap()
                .state = OperationState::Preparing;
            drop(state);
            let engine = self.clone();
            if let Ok(app) = self.app() {
                app.runtime.spawn(async move {
                    engine.run_queued(id, entry, sw).await;
                });
            }
        }
    }
    pub(super) fn refresh_queue_progress(&self) {
        let Ok(app) = self.app() else { return };
        let mut state = self.state.lock().unwrap();
        for op in &mut state.operations {
            if !matches!(op.state, OperationState::Running | OperationState::Stopping) {
                continue;
            }
            let (Some(id), Some(details)) = (op.device, op.transfer.as_mut()) else {
                continue;
            };
            let Some(sw) = app.switches.list().into_iter().find(|s| s.id == id) else {
                continue;
            };
            if sw.tracked_operation() != op.id {
                continue;
            }
            if let Some(t) = sw.transfer(&app.sessions).and_then(|t| t.session) {
                details.bytes = t.bytes;
                details.speed = t.current_speed;
                details.eta = t.eta();
            }
            if matches!(sw.upgrade(), upgrade::Progress::Verifying) {
                details.cancellable = false;
            }
        }
    }
    async fn run_queued(self: Arc<Self>, id: OperationId, entry: Entry, sw: Arc<Switch>) {
        let result = self.execute_queued(id, &entry, &sw).await;
        if let Work::Transfer(r) = &entry.work {
            if r.receive {
                if let Ok(app) = self.app() {
                    app.services.revoke_receive(&r.local);
                }
            }
        }
        if entry.cancel.load(Ordering::Acquire) {
            let state = if sw.copy_abort_confirmed(id) || !sw.tracked_started(id) {
                OperationState::Cancelled
            } else {
                OperationState::Uncertain("Abort requested. The device may retain a partial file; reconnect and inspect the destination before retrying.".into())
            };
            self.set_operation_state(id, state);
        } else if result.as_ref().err().is_some_and(|e| {
            sw.tracked_started(id)
                && matches!(
                    Failure::from_message(e).kind,
                    FailureKind::Network | FailureKind::RemoteUnknown
                )
        }) {
            self.set_operation_state(
                id,
                OperationState::Uncertain(format!(
                    "Transfer connection lost; destination may be incomplete. {}",
                    result.unwrap_err()
                )),
            );
            self.state.lock().unwrap().queue.pause(sw.id);
        } else {
            self.finish(id, result);
        }
        if sw.state().is_over() {
            self.state.lock().unwrap().queue.paused.insert(entry.device);
        }
    }
    async fn execute_queued(
        self: &Arc<Self>,
        id: OperationId,
        entry: &Entry,
        sw: &Arc<Switch>,
    ) -> Result<(), String> {
        if entry.cancel.load(Ordering::Acquire) {
            return Ok(());
        }
        // Refresh a download's source inventory before checking its frozen size.
        if let Work::Transfer(r) = &entry.work {
            if r.receive {
                let parent = r
                    .remote
                    .rsplit_once('/')
                    .map(|(p, _)| p.to_string())
                    .unwrap_or_else(|| format!("{}:", r.remote.split_once(':').unwrap().0));
                self.run_tracked(id, sw, Job::List { path: parent }).await?;
            }
        }
        let e = self.clone();
        let frozen = entry.clone();
        let stamp = tokio::task::spawn_blocking(move || e.check_entry(&frozen))
            .await
            .map_err(|e| e.to_string())??;
        if entry.stamp.as_ref() != Some(&stamp) {
            return Err("source file changed since this job was queued; select it again".into());
        }
        if entry.cancel.load(Ordering::Acquire) {
            return Ok(());
        }
        let app = self.app()?;
        if let Some(p) = entry.protocol {
            let service = cisco::service_of(p);
            if !app.services.status(service).is_running() {
                app.services.start(service);
            }
            let started = Instant::now();
            loop {
                if entry.cancel.load(Ordering::Acquire) {
                    return Ok(());
                }
                match app.services.status(service) {
                    ServiceStatus::Running => break,
                    ServiceStatus::Failed(e) => return Err(e),
                    _ if started.elapsed() > Duration::from_secs(15) => {
                        return Err("service did not start within 15 seconds".into())
                    }
                    _ => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            }
        }
        let job = match &entry.work {
            Work::Transfer(r) => {
                let e = self.clone();
                let r = r.clone();
                let s = sw.clone();
                let overwrite = r.overwrite;
                let platform_check = r.platform_check;
                let (mut job, size) = tokio::task::spawn_blocking(move || {
                    e.transfer_job(&s, &r.local, &r.remote, r.receive, r.protocol)
                })
                .await
                .map_err(|e| e.to_string())??;
                if let Job::Copy {
                    overwrite: allow,
                    platform_check: check,
                    ..
                } = &mut job
                {
                    *allow = overwrite;
                    *check = platform_check;
                }
                if let Work::Transfer(r) = &entry.work {
                    app.sessions
                        .allow_transfer(r.protocol, sw.transfer_peer(), &r.local);
                    if r.receive {
                        app.services.authorize_receive(
                            r.local.clone(),
                            sw.transfer_peer().ok_or("device address is unknown")?,
                        );
                        sw.begin_receive(r.local.clone(), size);
                    } else {
                        sw.begin_transfer(r.local.clone(), size, r.protocol);
                    }
                }
                job
            }
            Work::Image(job) => {
                let mut job = job.clone();
                if let Job::PrepareUpgrade {
                    protocol,
                    rel_path,
                    remote,
                    command,
                    ..
                } = &mut job
                {
                    let cfg = app.config.read().unwrap().clone();
                    let peer = sw.transfer_peer();
                    let ip = copy_endpoint(&cfg, *protocol, peer.as_ref())?.ip;
                    *command = cisco::deploy_command(*protocol, &cfg, &ip, rel_path, remote)?;
                    app.sessions
                        .allow_transfer(*protocol, sw.transfer_peer(), rel_path);
                }
                job
            }
        };
        let command = match &job {
            Job::Copy { command, .. } | Job::PrepareUpgrade { command, .. } => {
                Some(command.clone())
            }
            _ => None,
        };
        if let Some(command) = &command {
            let facts = sw.facts();
            let mut event = logging::Event::new(
                logging::LogLevel::Info,
                entry.protocol.map(|p| p.label()).unwrap_or("SSH"),
                "copy command",
            )
            .device(
                facts.hostname.clone().unwrap_or_else(|| sw.host.clone()),
                facts.version.as_ref().and_then(|v| v.model.clone()),
            )
            .command(command.clone());
            if let Some(ip) = sw.transfer_peer() {
                event = event.ip(ip);
            }
            app.logger.log(event);
        }
        {
            let mut state = self.state.lock().unwrap();
            if entry.cancel.load(Ordering::Acquire) {
                return Ok(());
            }
            let operation = state
                .operations
                .iter_mut()
                .find(|op| op.id == id)
                .ok_or("job no longer exists")?;
            operation.state = OperationState::Running;
            if let Some(transfer) = &mut operation.transfer {
                transfer.command = command;
            }
            if !sw.submit_cancellable(id, job, entry.cancel.clone()) {
                return Err("SSH session is closed".into());
            }
        }
        self.wait_tracked(id, sw).await
    }

    async fn run_tracked(&self, id: OperationId, sw: &Arc<Switch>, job: Job) -> Result<(), String> {
        if !sw.submit_tracked(id, job) {
            return Err("SSH session is closed".into());
        }
        self.wait_tracked(id, sw).await
    }
    async fn wait_tracked(&self, id: OperationId, sw: &Arc<Switch>) -> Result<(), String> {
        loop {
            if let Some(result) = sw.take_tracked_result(id) {
                return result.map(|_| ());
            }
            if sw.state().is_over() {
                return Err("device disconnected; reconnect and resume pending jobs".into());
            }
            if self.app.upgrade().is_none() {
                return Err("application is shutting down".into());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    pub(super) fn set_operation_state(&self, id: OperationId, status: OperationState) {
        if let Some(operation) = self
            .state
            .lock()
            .unwrap()
            .operations
            .iter_mut()
            .find(|o| o.id == id)
        {
            operation.state = status;
            let _ = self.events.send(AppEvent::Finished(operation.clone()));
        }
    }
    pub(super) fn cancel_operation(&self, id: OperationId) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        let entry = state
            .queue
            .entries
            .get(&id)
            .cloned()
            .ok_or("this operation cannot be cancelled")?;
        let operation = state
            .operations
            .iter_mut()
            .find(|o| o.id == id)
            .ok_or("job no longer exists")?;
        if operation.state == OperationState::Stopping {
            return Ok(());
        }
        if !operation.state.is_active() {
            return Err("job has already finished".into());
        }
        if !operation.transfer.as_ref().is_some_and(|t| t.cancellable) {
            return Err("verification / installation cannot be cancelled".into());
        }
        if operation.state == OperationState::Running {
            let sw = self.device(entry.device)?;
            if !sw.request_copy_abort(id)
                && (sw.tracked_started(id) || matches!(sw.upgrade(), upgrade::Progress::Verifying))
            {
                return Err("the copy has finished; wait for verification".into());
            }
            entry.cancel.store(true, Ordering::Release);
            operation.state = OperationState::Stopping;
            if let Some(t) = sw
                .transfer(&self.app()?.sessions)
                .filter(|_| sw.tracked_started(id))
            {
                self.app()?.sessions.abort_transfer(
                    t.protocol,
                    sw.transfer_peer(),
                    &t.rel_path,
                    t.started,
                );
            }
        } else {
            entry.cancel.store(true, Ordering::Release);
            operation.state = OperationState::Cancelled;
            state.queue.order.retain(|key| *key != id);
        }
        let _ = self.events.send(AppEvent::Changed);
        Ok(())
    }
    pub(super) fn retry_operation(
        self: &Arc<Self>,
        old: OperationId,
        id: OperationId,
        overwrite: bool,
    ) -> Result<(), String> {
        let (entry, details, label) = {
            let state = self.state.lock().unwrap();
            let op = state
                .operations
                .iter()
                .find(|o| o.id == old)
                .ok_or("job no longer exists")?;
            let origin = state
                .queue
                .entries
                .get(&old)
                .and_then(|e| e.retry_origin)
                .unwrap_or(old);
            if state.operations.iter().any(|o| {
                o.state.is_active()
                    && state
                        .queue
                        .entries
                        .get(&o.id)
                        .is_some_and(|e| e.retry_origin == Some(origin))
            }) {
                return Err("A retry for this job is already pending or running".into());
            }
            if !matches!(
                op.state,
                OperationState::Failed(_)
                    | OperationState::Cancelled
                    | OperationState::Uncertain(_)
            ) {
                return Err("only failed or cancelled jobs can be retried".into());
            }
            if matches!(op.state, OperationState::Uncertain(_))
                && !matches!(
                    op.transfer.as_ref().and_then(|t| t.inspection.as_ref()),
                    Some(
                        workflow::TransferInspection::Missing
                            | workflow::TransferInspection::Partial { .. }
                    )
                )
            {
                return Err(
                    "Inspect the destination before retrying a transfer with unknown remote status"
                        .into(),
                );
            }
            if !overwrite
                && matches!(
                    op.transfer.as_ref().and_then(|t| t.inspection.as_ref()),
                    Some(workflow::TransferInspection::Partial { .. })
                )
            {
                return Err(
                    "Destination differs; use overwrite and retry with explicit y confirmation"
                        .into(),
                );
            }
            (
                state
                    .queue
                    .entries
                    .get(&old)
                    .cloned()
                    .ok_or("job cannot be retried")?,
                op.transfer.clone().unwrap(),
                op.label.clone(),
            )
        };
        // Keep the original stamp: retry must never silently upload a changed file.
        let original_stamp = entry.stamp.clone();
        let mut entry = entry;
        let origin = entry.retry_origin.unwrap_or(old);
        entry.retry_origin = Some(origin);
        entry.cancel = Arc::new(AtomicBool::new(false));
        if overwrite {
            match &mut entry.work {
                Work::Transfer(request) => request.overwrite = true,
                Work::Image(Job::PrepareUpgrade { overwrite, .. }) => *overwrite = true,
                _ => {}
            }
        }
        let mut details = details;
        details.bytes = 0;
        details.command = None;
        details.inspection = None;
        details.speed = 0.0;
        details.eta = None;
        details.cancellable = matches!(&entry.work, Work::Transfer(_)) || entry.protocol.is_some();
        self.add_queued(id, entry, details, &label);
        {
            let mut state = self.state.lock().unwrap();
            // A workflow follows the latest attempt; earlier attempts remain in history.
            let lineage: std::collections::HashSet<_> = state
                .queue
                .entries
                .iter()
                .filter(|(key, entry)| **key == origin || entry.retry_origin == Some(origin))
                .map(|(key, _)| *key)
                .collect();
            let mut reopened = Vec::new();
            for workflow in &mut state.workflows {
                if workflow.jobs.iter().any(|job| lineage.contains(job)) {
                    workflow.jobs.retain(|job| !lineage.contains(job));
                    workflow.jobs.push(id);
                    workflow.finished = None;
                    reopened.push(workflow.id);
                }
            }
            for workflow in reopened {
                state.workflow_reports.remove(&workflow);
            }
        }
        if let Some(stamp) = original_stamp {
            self.state
                .lock()
                .unwrap()
                .queue
                .entries
                .get_mut(&id)
                .unwrap()
                .stamp = Some(stamp);
        }
        Ok(())
    }
    pub(super) fn resume_queue(&self, device: DeviceId) -> Result<(), String> {
        if self.device(device)?.state() != SwitchState::Ready {
            return Err("reconnect the device before resuming jobs".into());
        }
        let mut state = self.state.lock().unwrap();
        state.queue.paused.remove(&device);
        for op in &mut state.operations {
            if op.device == Some(device) && op.state == OperationState::Paused {
                op.state = OperationState::Queued;
            }
        }
        Ok(())
    }
}

impl Engine {
    pub(super) fn review_pending(
        self: &Arc<Self>,
        devices: Vec<DeviceId>,
        id: OperationId,
    ) -> Result<(), String> {
        let entries: Vec<_> = {
            let state = self.state.lock().unwrap();
            state
                .queue
                .entries
                .iter()
                .filter(|(key, e)| {
                    devices.contains(&e.device)
                        && state
                            .operations
                            .iter()
                            .any(|o| o.id == **key && o.state == OperationState::Paused)
                })
                .map(|(key, e)| (*key, e.clone(), state.network.revision))
                .collect()
        };
        if entries.is_empty() {
            return Err("No paused jobs to review".into());
        }
        self.new_operation(id, None, "review pending jobs");
        let engine = self.clone();
        let app = self.app()?;
        app.runtime.spawn(async move {
            let reviews = tokio::task::spawn_blocking({
                let engine = engine.clone();
                move || {
                    entries
                        .into_iter()
                        .map(|(operation, entry, revision)| {
                            let result = (|| -> Result<(), String> {
                                let sw = engine.device(entry.device)?;
                                if sw.state() != SwitchState::Ready {
                                    return Err("Reconnect the device first".into());
                                }
                                let stamp = engine.check_entry(&entry)?;
                                if entry.stamp.as_ref() != Some(&stamp) {
                                    return Err("Source file changed; select it again".into());
                                }
                                if let Some(protocol) = entry.protocol {
                                    let cfg = engine.app()?.config.read().unwrap().clone();
                                    copy_endpoint(&cfg, protocol, sw.transfer_peer().as_ref())?;
                                }
                                Ok(())
                            })();
                            workflow::QueueReview {
                                operation,
                                device: entry.device,
                                revision,
                                ready: result.is_ok(),
                                message: result.err().unwrap_or_else(|| {
                                    "Source and interface checked · ready for explicit resume"
                                        .into()
                                }),
                            }
                        })
                        .collect::<Vec<_>>()
                }
            })
            .await;
            match reviews {
                Ok(reviews) => {
                    {
                        let mut state = engine.state.lock().unwrap();
                        state.queue_reviews = reviews;
                        state.workflow_revision += 1;
                    }
                    engine.finish(id, Ok(()));
                }
                Err(error) => engine.finish(id, Err(error.to_string())),
            }
        });
        Ok(())
    }
    pub(super) fn resume_reviewed(&self, devices: Vec<DeviceId>) -> Result<(), String> {
        let state = self.state.lock().unwrap();
        for op in state.operations.iter().filter(|o| {
            o.state == OperationState::Paused && o.device.is_some_and(|d| devices.contains(&d))
        }) {
            if !state
                .queue_reviews
                .iter()
                .any(|r| r.operation == op.id && r.ready && r.revision == state.network.revision)
            {
                return Err(
                    "Review paused jobs after the latest network change before resuming".into(),
                );
            }
        }
        drop(state);
        for device in &devices {
            if self.device(*device)?.state() != SwitchState::Ready {
                return Err("Reconnect all selected devices first".into());
            }
        }
        for device in devices {
            self.resume_queue(device)?;
        }
        Ok(())
    }
    pub(super) fn inspect_operation(
        self: &Arc<Self>,
        old: OperationId,
        id: OperationId,
    ) -> Result<(), String> {
        let (entry, details) = {
            let state = self.state.lock().unwrap();
            let op = state
                .operations
                .iter()
                .find(|o| o.id == old)
                .ok_or("Job no longer exists")?;
            if !matches!(
                op.state,
                OperationState::Failed(_)
                    | OperationState::Uncertain(_)
                    | OperationState::Cancelled
            ) {
                return Err("Only interrupted jobs can be inspected".into());
            }
            (
                state
                    .queue
                    .entries
                    .get(&old)
                    .cloned()
                    .ok_or("Original source is no longer available")?,
                op.transfer.clone().ok_or("Select a copy job")?,
            )
        };
        let sw = self.device(entry.device)?;
        if sw.state() != SwitchState::Ready {
            return Err("Reconnect and wait until the device is ready".into());
        }
        if self.state.lock().unwrap().operations.iter().any(|o| {
            o.device == Some(sw.id)
                && matches!(o.state, OperationState::Preparing | OperationState::Running)
        }) {
            return Err("Wait until other device work finishes before inspecting".into());
        }
        self.new_operation(id, Some(sw.id), "inspect destination");
        let engine = self.clone();
        let app = self.app()?;
        app.runtime.spawn(async move {
            let inspection = async {
                let frozen = entry.clone();
                let check = engine.clone();
                let stamp = tokio::task::spawn_blocking(move || check.inspection_stamp(&frozen))
                    .await
                    .map_err(|e| e.to_string())??;
                if entry.stamp.as_ref() != Some(&stamp) {
                    return Err("Source file changed; select it again".into());
                }
                let path = fsroot::SecureRoot::new(&entry.root)
                    .map_err(|e| e.to_string())?
                    .resolve_for_write(&details.local)
                    .map_err(|e| e.to_string())?;
                engine
                    .run_tracked(
                        id,
                        &sw,
                        Job::InspectTransfer {
                            local_path: path,
                            remote: details.remote.clone(),
                            expected_size: details.size,
                            receive: details.receive,
                            upgrade_image: matches!(entry.work, Work::Image(_)),
                        },
                    )
                    .await?;
                if !details.receive
                    && entry.stamp.as_ref() != Some(&engine.inspection_stamp(&entry)?)
                {
                    sw.set_upgrade(upgrade::Progress::Failed(
                        "Source changed during destination inspection".into(),
                    ));
                    return Err(
                        "Source changed during destination inspection; select it again".into(),
                    );
                }
                Ok::<_, String>(sw.take_inspection().unwrap_or(
                    workflow::TransferInspection::Unknown(
                        "Device did not return an inspection result".into(),
                    ),
                ))
            }
            .await;
            match inspection {
                Ok(result) => {
                    let verified = result == workflow::TransferInspection::Verified;
                    let mut state = engine.state.lock().unwrap();
                    if let Some(op) = state.operations.iter_mut().find(|o| o.id == old) {
                        if let Some(t) = &mut op.transfer {
                            t.inspection = Some(result);
                        }
                        if verified {
                            op.state = OperationState::Complete;
                        }
                    }
                    let workflows: Vec<_> = state
                        .workflows
                        .iter()
                        .filter(|w| w.jobs.contains(&old))
                        .map(|w| w.id)
                        .collect();
                    for workflow in workflows {
                        state.workflow_reports.remove(&workflow);
                        if let Some(w) = state.workflows.iter_mut().find(|w| w.id == workflow) {
                            w.finished = None;
                        }
                    }
                    state.workflow_revision += 1;
                    drop(state);
                    engine.finish(id, Ok(()));
                    engine.finish_workflows();
                }
                Err(error) => {
                    if let Some(t) = engine
                        .state
                        .lock()
                        .unwrap()
                        .operations
                        .iter_mut()
                        .find(|o| o.id == old)
                        .and_then(|o| o.transfer.as_mut())
                    {
                        t.inspection = Some(workflow::TransferInspection::Unknown(error.clone()));
                    }
                    engine.finish(id, Err(error));
                }
            }
        });
        Ok(())
    }
    pub(super) fn overwrite_retry(
        self: &Arc<Self>,
        old: OperationId,
        id: OperationId,
    ) -> Result<(), String> {
        self.retry_operation(old, id, true)
    }
    fn inspection_stamp(&self, entry: &Entry) -> Result<Stamp, String> {
        let local = match &entry.work {
            Work::Transfer(r) if r.receive => {
                return entry
                    .stamp
                    .clone()
                    .ok_or("Original remote source was not recorded".into())
            }
            Work::Transfer(r) => &r.local,
            Work::Image(
                Job::PrepareUpgrade { rel_path, .. } | Job::VerifyUpgrade { rel_path, .. },
            ) => rel_path,
            _ => return Err("Not a transfer job".into()),
        };
        let path = fsroot::SecureRoot::new(&entry.root)
            .map_err(|e| e.to_string())?
            .resolve(local)
            .map_err(|e| e.to_string())?;
        let meta = path.metadata().map_err(|e| e.to_string())?;
        Ok(Stamp {
            size: meta.len(),
            modified: meta.modified().ok(),
        })
    }
}
