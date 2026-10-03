//! Network supervision and recovery shared by both frontends. Never replays work.
use super::*;
use crate::netif::{self, NetInterface};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    Network,
    Negotiation,
    Authentication,
    HostKey,
    Service,
    SourceChanged,
    RemoteUnknown,
    Validation,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub kind: FailureKind,
    pub message: String,
    pub remedy: &'static str,
}
impl Failure {
    pub fn from_message(message: &str) -> Self {
        let text = message.to_lowercase();
        let (kind, remedy) = if text.contains("login failed")
            || text.contains("authentication limit")
            || text.contains("permission denied")
            || text.contains("authentication failed")
            || text.contains("authentication rejected")
            || text.contains("login rejected")
        {
            (
                FailureKind::Authentication,
                "Edit credentials and reconnect; automatic login attempts are stopped.",
            )
        } else if text.contains("host key") {
            (FailureKind::HostKey, "Review the device's SSH host key.")
        } else if text.contains("negotiation") && !text.contains("before ssh negotiation") {
            (
                FailureKind::Negotiation,
                "Run Test SSH transport to inspect the algorithms offered by the device.",
            )
        } else if text.contains("source file changed") {
            (
                FailureKind::SourceChanged,
                "Select the changed source again to create a new job.",
            )
        } else if text.contains("local address")
            || text.contains("interface")
            || text.contains("no route")
            || text.contains("timed out")
            || text.contains("connection")
            || text.contains("disconnected")
            || text.contains("session closed")
            || text.contains("closed the session")
            || text.contains("no answer")
        {
            (FailureKind::Network, "Check VPN, route and the Copy URL interface; reconnect, then inspect the destination.")
        } else if text.contains("service")
            || text.contains("bind")
            || text.contains("port ")
            || text.contains("port:")
            || text.contains("listener")
        {
            (
                FailureKind::Service,
                "Check the selected service and its bind address in Dashboard.",
            )
        } else if text.contains("unknown")
            || text.contains("abort requested")
            || text.contains("partial")
        {
            (
                FailureKind::RemoteUnknown,
                "Inspect the destination before transferring or installing again.",
            )
        } else {
            (
                FailureKind::Validation,
                "Correct the reported prerequisite and try again.",
            )
        };
        Self {
            kind,
            message: message.into(),
            remedy,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryPhase {
    Connected,
    Waiting,
    Reconnecting,
    ReviewRequired,
    LoginRequired,
    Stopped,
}
#[derive(Debug, Clone)]
pub struct RecoveryStatus {
    pub phase: RecoveryPhase,
    pub attempts: u32,
    pub next_attempt: Option<Instant>,
    pub reason: Option<Failure>,
    pub established: bool,
}
impl Default for RecoveryStatus {
    fn default() -> Self {
        Self {
            phase: RecoveryPhase::Connected,
            attempts: 0,
            next_attempt: None,
            reason: None,
            established: false,
        }
    }
}
impl RecoveryStatus {
    pub fn label(&self) -> &'static str {
        match self.phase {
            RecoveryPhase::Connected => "Connected",
            RecoveryPhase::Waiting => "Waiting for network",
            RecoveryPhase::Reconnecting => "Reconnecting",
            RecoveryPhase::ReviewRequired => "Connected · review pending jobs",
            RecoveryPhase::LoginRequired => "Login needs attention",
            RecoveryPhase::Stopped => "Automatic reconnect stopped",
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetworkSnapshot {
    pub interfaces: Vec<NetInterface>,
    pub routes: std::collections::BTreeMap<DeviceId, Option<std::net::IpAddr>>,
    pub revision: u64,
    pub message: Option<String>,
}
#[derive(Debug, Clone)]
pub struct CopyEndpoint {
    pub ip: std::net::IpAddr,
    pub interface: Option<String>,
    pub fixed_bind: bool,
}

pub fn copy_endpoint(
    cfg: &config::Config,
    protocol: Protocol,
    peer: Option<&std::net::IpAddr>,
) -> Result<CopyEndpoint, String> {
    let bind = &cfg.service(cisco::service_of(protocol)).bind;
    let fixed_bind = bind
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| !ip.is_unspecified());
    let ip = cfg.advertised_ip(bind, peer).ok_or_else(|| {
        format!(
            "Copy URL interface {} is unavailable; select another interface",
            cfg.advertise.as_deref().unwrap_or("Automatic")
        )
    })?;
    let interface = netif::interface_of(&ip);
    if interface.is_none() {
        return Err(format!(
            "Copy URL address {ip} is no longer local; correct the {} service bind or interface",
            protocol.label()
        ));
    }
    Ok(CopyEndpoint {
        ip,
        interface,
        fixed_bind,
    })
}
/// Resolve the copy address from the supervisor's cached network facts only.
/// Rendering callers must never enumerate interfaces or open route sockets.
pub fn cached_copy_endpoint(
    bind: &str,
    advertise: Option<&str>,
    device: Option<DeviceId>,
    network: &NetworkSnapshot,
) -> Result<CopyEndpoint, String> {
    let fixed = bind
        .parse::<std::net::IpAddr>()
        .ok()
        .filter(|ip| !ip.is_unspecified());
    let ip = if let Some(ip) = fixed {
        Some(ip)
    } else if let Some(choice) = advertise {
        let choice = choice.trim();
        if let Ok(ip) = choice.parse::<std::net::IpAddr>() {
            network.interfaces.iter().find(|i| i.ip == ip).map(|i| i.ip)
        } else {
            network
                .interfaces
                .iter()
                .find(|i| i.name == choice && i.ip.is_ipv4())
                .or_else(|| network.interfaces.iter().find(|i| i.name == choice))
                .map(|i| i.ip)
        }
    } else {
        device.and_then(|id| network.routes.get(&id).copied().flatten())
    }
    .ok_or_else(|| {
        if advertise.is_none() && device.is_none() {
            "Automatic · per device".to_owned()
        } else {
            "Copy URL interface or route is unavailable".to_owned()
        }
    })?;
    let interface = network
        .interfaces
        .iter()
        .find(|i| i.ip == ip)
        .map(|i| i.name.clone())
        .ok_or("Copy URL address is no longer local")?;
    Ok(CopyEndpoint {
        ip,
        interface: Some(interface),
        fixed_bind: fixed.is_some(),
    })
}

fn backoff(attempts: u32) -> Duration {
    Duration::from_secs(match attempts {
        0 => 2,
        1 => 5,
        2 => 10,
        3 => 20,
        _ => 30,
    })
}

impl Engine {
    pub(super) fn start_recovery(self: &Arc<Self>, app: &SharedApp) {
        let weak = Arc::downgrade(self);
        app.runtime.spawn(async move {
            let mut network_due = Instant::now();
            loop {
                let Some(engine) = weak.upgrade() else { break };
                let Ok(app) = engine.app() else { break };
                if engine.stopping.load(Ordering::Acquire) {
                    break;
                }
                if Instant::now() >= network_due {
                    let peers: Vec<_> = app
                        .switches
                        .list()
                        .into_iter()
                        .filter_map(|sw| sw.transfer_peer().map(|peer| (sw.id, peer)))
                        .collect();
                    if let Ok((interfaces, routes)) = tokio::task::spawn_blocking(move || {
                        (
                            netif::interfaces(),
                            peers
                                .into_iter()
                                .map(|(id, peer)| (id, netif::source_ip_for(&peer)))
                                .collect(),
                        )
                    })
                    .await
                    {
                        engine.network_changed(interfaces, routes);
                    }
                    let active = !app.switches.list().is_empty()
                        || ServiceId::ALL
                            .into_iter()
                            .any(|id| app.services.status(id).is_running());
                    network_due = Instant::now() + Duration::from_secs(if active { 2 } else { 15 });
                }
                engine.recovery_tick();
                drop(app);
                drop(engine);
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
    }
    pub(super) fn network_changed(
        &self,
        mut interfaces: Vec<NetInterface>,
        routes: std::collections::BTreeMap<DeviceId, Option<std::net::IpAddr>>,
    ) {
        interfaces.sort_by_key(|i| (i.name.clone(), i.ip.to_string()));
        let mut state = self.state.lock().unwrap();
        if state.network.interfaces == interfaces && state.network.routes == routes {
            return;
        }
        let changed = state.network_observed
            && (state.network.interfaces != interfaces
                || state
                    .network
                    .routes
                    .iter()
                    .any(|(id, old)| routes.get(id).is_some_and(|new| old != new)));
        state.network_observed = true;
        state.network.interfaces = interfaces;
        state.network.routes = routes;
        state.network.revision += 1;
        if changed {
            state.network.message = Some("Laptop network changed. Review the Copy URL interface before resuming pending jobs.".into());
            let devices: Vec<_> = state
                .operations
                .iter()
                .filter(|o| matches!(o.state, OperationState::Queued | OperationState::Paused))
                .filter_map(|o| o.device)
                .collect();
            for device in devices {
                state.queue.pause(device);
            }
            for op in &mut state.operations {
                if matches!(op.state, OperationState::Queued) {
                    op.state = OperationState::Paused;
                }
            }
        }
        let _ = self.events.send(AppEvent::Changed);
    }
    pub(super) fn recovery_tick(&self) {
        let Ok(app) = self.app() else { return };
        let devices = app.switches.list();
        let mut reconnect = Vec::new();
        let mut state = self.state.lock().unwrap();
        state
            .recoveries
            .retain(|id, _| devices.iter().any(|d| d.id == *id));
        for sw in devices {
            let status = sw.state();
            let paused = state.operations.iter().any(|o| {
                o.device == Some(sw.id)
                    && matches!(
                        o.state,
                        OperationState::Paused | OperationState::Uncertain(_)
                    )
            });
            let recovery = state.recoveries.entry(sw.id).or_default();
            if status.is_live() {
                recovery.established = true;
                if status == SwitchState::Ready
                    && matches!(
                        recovery.phase,
                        RecoveryPhase::Reconnecting | RecoveryPhase::Waiting
                    )
                {
                    recovery.phase = if paused {
                        RecoveryPhase::ReviewRequired
                    } else {
                        RecoveryPhase::Connected
                    };
                    recovery.next_attempt = None;
                }
                continue;
            }
            if matches!(
                status,
                SwitchState::Connecting | SwitchState::HostKey { .. }
            ) {
                continue;
            }
            if matches!(status, SwitchState::Closed) || sw.cancel_requested() {
                recovery.phase = RecoveryPhase::Stopped;
                recovery.next_attempt = None;
                continue;
            }
            let reason = match status {
                SwitchState::Offline { reason } | SwitchState::Failed { reason } => reason,
                _ => continue,
            };
            let failure = Failure::from_message(&reason);
            if matches!(
                failure.kind,
                FailureKind::Authentication | FailureKind::Negotiation | FailureKind::HostKey
            ) {
                recovery.phase = RecoveryPhase::LoginRequired;
                recovery.reason = Some(failure);
                recovery.next_attempt = None;
                continue;
            }
            if !recovery.established
                || !sw.can_reconnect()
                || recovery.phase == RecoveryPhase::LoginRequired
            {
                continue;
            }
            recovery.reason = Some(failure);
            let now = Instant::now();
            if recovery.next_attempt.is_none() {
                recovery.phase = RecoveryPhase::Waiting;
                recovery.next_attempt = Some(now + backoff(recovery.attempts));
            }
            if recovery.next_attempt.is_some_and(|due| now >= due) {
                recovery.attempts += 1;
                recovery.phase = RecoveryPhase::Reconnecting;
                recovery.next_attempt = None;
                reconnect.push(sw);
            }
        }
        drop(state);
        for sw in reconnect {
            app.switches.reconnect(sw);
        }
    }
    pub(super) fn reset_recovery(&self, device: DeviceId) {
        self.state.lock().unwrap().recoveries.remove(&device);
    }
}

#[cfg(test)]
mod endpoint_tests {
    use super::*;
    fn network() -> NetworkSnapshot {
        NetworkSnapshot {
            interfaces: vec![
                NetInterface {
                    name: "en0".into(),
                    ip: "10.0.0.2".parse().unwrap(),
                    kind: crate::netif::IfKind::Physical,
                },
                NetInterface {
                    name: "utun4".into(),
                    ip: "192.0.2.2".parse().unwrap(),
                    kind: crate::netif::IfKind::Tunnel,
                },
            ],
            routes: [(1, Some("192.0.2.2".parse().unwrap()))].into(),
            ..Default::default()
        }
    }
    #[test]
    fn cached_endpoints_respect_route_pin_bind_and_unavailable_states() {
        let mut facts = network();
        let endpoint = cached_copy_endpoint("0.0.0.0", None, Some(1), &facts).unwrap();
        assert_eq!(endpoint.ip.to_string(), "192.0.2.2");
        assert_eq!(endpoint.interface.as_deref(), Some("utun4"));
        assert_eq!(
            cached_copy_endpoint("0.0.0.0", Some(" en0 "), Some(1), &facts)
                .unwrap()
                .ip
                .to_string(),
            "10.0.0.2"
        );
        assert_eq!(
            cached_copy_endpoint("0.0.0.0", Some(" 10.0.0.2 "), Some(1), &facts)
                .unwrap()
                .ip
                .to_string(),
            "10.0.0.2"
        );
        assert!(cached_copy_endpoint("0.0.0.0", None, None, &facts).is_err());
        assert!(cached_copy_endpoint("0.0.0.0", None, Some(2), &facts).is_err());
        let endpoint = cached_copy_endpoint("10.0.0.2", Some("missing"), Some(1), &facts).unwrap();
        assert!(endpoint.fixed_bind);
        assert_eq!(endpoint.ip.to_string(), "10.0.0.2");
        assert!(cached_copy_endpoint("10.0.0.9", Some("en0"), Some(1), &facts).is_err());
        facts.interfaces[0].ip = "10.0.0.3".parse().unwrap();
        assert_eq!(
            cached_copy_endpoint("0.0.0.0", Some("en0"), Some(1), &facts)
                .unwrap()
                .ip
                .to_string(),
            "10.0.0.3"
        );
        assert!(cached_copy_endpoint("0.0.0.0", Some("10.0.0.2"), Some(1), &facts).is_err());
        facts.interfaces.clear();
        assert!(cached_copy_endpoint("0.0.0.0", Some("en0"), Some(1), &facts).is_err());
    }
}
