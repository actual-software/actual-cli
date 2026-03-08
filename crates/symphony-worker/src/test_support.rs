/// Shared mock types for symphony-worker tests.
///
/// These are in a separate file because `mockall::mock!` macro expansions
/// generate struct definitions that LLVM assigns coverage counters to.
/// On Linux, these counters are never "hit" (struct defs are declarations,
/// not executable code), causing phantom 0-count lines. By isolating the
/// mocks here and excluding this file from coverage measurement, we avoid
/// false negatives without losing meaningful coverage.
use async_trait::async_trait;
use mockall::mock;

use crate::executor::{AgentHandle, AgentLauncher};
use symphony::protocol::AgentEvent;

mock! {
    pub TestAgentHandle {}

    #[async_trait]
    impl AgentHandle for TestAgentHandle {
        async fn wait_with_timeout(&mut self, timeout_ms: u64) -> Result<bool, String>;
        async fn kill(&mut self);
    }
}

mock! {
    pub TestAgentLauncher {}

    #[async_trait]
    impl AgentLauncher for TestAgentLauncher {
        async fn launch_agent(
            &self,
            workspace_path: &std::path::Path,
            prompt: &str,
            issue_identifier: &str,
        ) -> Result<(Box<dyn AgentHandle>, tokio::sync::mpsc::Receiver<AgentEvent>), String>;
    }
}
