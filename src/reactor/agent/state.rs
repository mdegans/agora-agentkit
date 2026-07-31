use serde::{Serialize, de::DeserializeOwned};

/// Persistent agent state. `Send + Sync` so the reactor can persist a snapshot
/// (`save(&snapshot())`) across an `.await`.
pub trait State: Serialize + DeserializeOwned + Send + Sync {}
