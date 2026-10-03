//! Workflow order and names shared by both interactive frontends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceView {
    Dashboard,
    Connect,
    Transfer,
    Upgrade,
    Logs,
}
impl WorkspaceView {
    pub const ALL: [Self; 5] = [
        Self::Dashboard,
        Self::Connect,
        Self::Transfer,
        Self::Upgrade,
        Self::Logs,
    ];
    pub const fn title(self) -> &'static str {
        match self {
            Self::Dashboard => "1 Dashboard",
            Self::Connect => "2 Connect",
            Self::Transfer => "3 Transfer",
            Self::Upgrade => "4 Upgrade",
            Self::Logs => "5 Logs",
        }
    }
    pub const fn name(self) -> &'static str {
        match self {
            Self::Dashboard => "Dashboard",
            Self::Connect => "Connect",
            Self::Transfer => "Transfer",
            Self::Upgrade => "Upgrade",
            Self::Logs => "Logs",
        }
    }
}
