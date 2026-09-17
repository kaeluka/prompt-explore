//! Run failures. Successful investigation evidence is returned as a trace;
//! failures retain their live progress evidence for the caller to inspect.

use serde::{Deserialize, Serialize};

/// A failure while running one scenario. `stage` identifies the runtime layer
/// (`"runner"` for PUT execution, input resolution, or tool simulation) and
/// `error` is its diagnostic text.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RunFailure {
    pub stage: String,
    pub error: String,
}
