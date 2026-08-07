use std::sync::{Arc, Mutex};

use rig_agent::agent::run::AgentRun;

#[derive(Clone, Debug, Default)]
pub struct CancellationTelemetry {
    state: Arc<Mutex<Option<String>>>,
}

impl CancellationTelemetry {
    pub fn checkpoint(&self, run: &AgentRun) -> serde_json::Result<()> {
        let serialized = serde_json::to_string(run)?;
        match self.state.lock() {
            Ok(mut state) => *state = Some(serialized),
            Err(poisoned) => *poisoned.into_inner() = Some(serialized),
        }
        Ok(())
    }

    pub fn serialized_state(&self) -> Option<String> {
        match self.state.lock() {
            Ok(state) => state.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}
