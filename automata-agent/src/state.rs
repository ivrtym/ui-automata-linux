//! Thin MCP Server state wrapping the unified `AutomationService`.

use std::path::PathBuf;
use std::sync::Arc;

use automata_linux::ConnectionMode;
use automata_service::{AutomationService, ServiceConfig};

pub struct AgentState {
    pub service: AutomationService,
}

impl AgentState {
    #[cfg(test)]
    pub fn new(
        default_connection_mode: ConnectionMode,
        workflows_dir: Option<PathBuf>,
    ) -> Arc<Self> {
        Self::with_file_policy(default_connection_mode, workflows_dir, true)
    }

    pub fn with_file_policy(
        default_connection_mode: ConnectionMode,
        workflows_dir: Option<PathBuf>,
        allow_file_writes: bool,
    ) -> Arc<Self> {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let resolved_wf_dir = workflows_dir
            .or_else(|| std::env::var_os("AUTOMATA_WORKFLOWS_DIR").map(PathBuf::from))
            .unwrap_or_else(|| {
                let candidate = PathBuf::from("workflows");
                if candidate.is_dir() {
                    candidate
                } else {
                    PathBuf::from(&home).join(".ui-automata").join("workflows")
                }
            });
        let logs_dir = PathBuf::from(&home).join(".ui-automata").join("logs");

        let cfg = ServiceConfig {
            connection_mode: default_connection_mode.into(),
            workflows_dir: resolved_wf_dir,
            logs_dir,
            allow_file_writes,
        };
        let service = AutomationService::open(cfg).expect("open AutomationService for MCP agent");

        Arc::new(Self { service })
    }

    pub fn list_resource_names(&self) -> Vec<String> {
        self.service.list_resource_names()
    }

    pub fn read_resource_content(&self, name: &str) -> anyhow::Result<String> {
        self.service
            .read_resource_content(name)
            .map_err(anyhow::Error::msg)
    }
}
