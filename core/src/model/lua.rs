//! Resource controls for caller-supplied Lua tool implementations.
//!
//! These are plain data (no runtime, no mlua): the scenario definition carries
//! them, the sandbox in `simulate::lua` enforces them. Hard ceilings are
//! process-safety boundaries, not defaults: API callers may lower limits or
//! raise defaults only this far, and direct library callers get the same
//! validation.

use std::time::Instant;

use serde::{Deserialize, Serialize};

pub const MAX_MEMORY_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_INSTRUCTIONS: u64 = 10_000_000;
pub const MAX_HOST_CALLS: usize = 1024;
pub const MAX_HOST_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;
pub const MAX_DURATION_MS: u64 = 10_000;
pub const MAX_VALUE_DEPTH: usize = 128;
pub const MAX_RESULT_BYTES: usize = 4 * 1024 * 1024;

/// Resource controls for one Lua tool-handler invocation.
///
/// All fields are explicit, documented overrides. Zero is invalid even for
/// direct library callers. The duration limit is cooperative: it is checked by
/// the VM hook and around Rust/Lua conversion boundaries, but a single native
/// Lua C operation cannot be preempted until it returns.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct LuaOptions {
    /// Lua allocator limit, including handler-created values (default 16 MiB).
    #[schema(minimum = 1, maximum = 67108864)]
    pub max_memory_bytes: usize,
    /// Shared VM/conversion work budget across initialization and handler execution (default 1 million).
    #[schema(minimum = 1, maximum = 10000000)]
    pub max_instructions: u64,
    /// Workspace capability-call limit (default 128).
    #[schema(minimum = 1, maximum = 1024)]
    pub max_host_calls: usize,
    /// Cumulative serialized workspace capability argument/result traffic limit (default 8 MiB).
    #[schema(minimum = 1, maximum = 33554432)]
    pub max_host_bytes: usize,
    /// UTF-8 source limit before parsing; bytecode is never loaded (default 256 KiB).
    #[schema(minimum = 1, maximum = 1048576)]
    pub max_source_bytes: usize,
    /// Cooperative wall-clock deadline for the whole invocation (default 2000 ms).
    #[schema(minimum = 1, maximum = 10000)]
    pub max_duration_ms: u64,
    /// Maximum JSON/Lua nesting depth (default 128; hard ceiling 128 for host stack safety).
    #[schema(minimum = 1, maximum = 128)]
    pub max_value_depth: usize,
    /// Maximum serialized size per converted value, shared by response and state-patch values (default 1 MiB).
    #[schema(minimum = 1, maximum = 4194304)]
    pub max_result_bytes: usize,
}

impl Default for LuaOptions {
    fn default() -> Self {
        Self {
            max_memory_bytes: 16 * 1024 * 1024,
            max_instructions: 1_000_000,
            max_host_calls: 128,
            max_host_bytes: 8 * 1024 * 1024,
            max_source_bytes: 256 * 1024,
            max_duration_ms: 2_000,
            max_value_depth: MAX_VALUE_DEPTH,
            max_result_bytes: 1024 * 1024,
        }
    }
}

impl LuaOptions {
    /// Validate limits for both API and standalone callers. No zero means
    /// unlimited, and callers cannot disable host stack protection.
    pub fn validate(&self) -> Result<(), String> {
        validate(self)
    }
}

pub fn validate(options: &LuaOptions) -> Result<(), String> {
    for (name, value, maximum) in [
        (
            "max_memory_bytes",
            options.max_memory_bytes as u64,
            MAX_MEMORY_BYTES as u64,
        ),
        (
            "max_instructions",
            options.max_instructions,
            MAX_INSTRUCTIONS,
        ),
        (
            "max_host_calls",
            options.max_host_calls as u64,
            MAX_HOST_CALLS as u64,
        ),
        (
            "max_host_bytes",
            options.max_host_bytes as u64,
            MAX_HOST_BYTES as u64,
        ),
        (
            "max_source_bytes",
            options.max_source_bytes as u64,
            MAX_SOURCE_BYTES as u64,
        ),
        ("max_duration_ms", options.max_duration_ms, MAX_DURATION_MS),
        (
            "max_value_depth",
            options.max_value_depth as u64,
            MAX_VALUE_DEPTH as u64,
        ),
        (
            "max_result_bytes",
            options.max_result_bytes as u64,
            MAX_RESULT_BYTES as u64,
        ),
    ] {
        if value > maximum {
            return Err(format!("Lua option '{name}' must not exceed {maximum}"));
        }
    }
    if Instant::now()
        .checked_add(std::time::Duration::from_millis(options.max_duration_ms))
        .is_none()
    {
        return Err("Lua option 'max_duration_ms' exceeds the platform clock range".into());
    }
    for (name, zero) in [
        ("max_memory_bytes", options.max_memory_bytes == 0),
        ("max_instructions", options.max_instructions == 0),
        ("max_host_calls", options.max_host_calls == 0),
        ("max_host_bytes", options.max_host_bytes == 0),
        ("max_source_bytes", options.max_source_bytes == 0),
        ("max_duration_ms", options.max_duration_ms == 0),
        ("max_value_depth", options.max_value_depth == 0),
        ("max_result_bytes", options.max_result_bytes == 0),
    ] {
        if zero {
            return Err(format!("Lua option '{name}' must be greater than zero"));
        }
    }
    Ok(())
}
