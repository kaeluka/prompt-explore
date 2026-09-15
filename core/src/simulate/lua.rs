//! Sandboxed Lua tool handlers.
//!
//! This is intentionally not a general Lua embedding. A handler gets only a
//! copied world state and accounted-for workspace capabilities; it has no host
//! filesystem, network, clock, or entropy access.

use std::{
    cell::RefCell, collections::HashSet, error::Error as StdError, fmt, rc::Rc, time::Instant,
};

use mlua::{
    ChunkMode, Error as LuaError, HookTriggers, Lua, LuaOptions as MluaOptions, StdLib, Table,
    Value as LuaValue, VmState,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::model::{
    SideEffect, ToolSchema,
    simulation::{ToolCall, WorkspaceOp},
};

use super::workspace::Workspace;

/// Conventional workspace-relative path for a generated Lua handler module.
pub const PROGRAM_PATH: &str = ".prompt-explore/tools.lua";
/// Hard ceilings are process-safety boundaries, not defaults. API callers may
/// lower limits or raise defaults only this far; direct library callers get the
/// same validation.
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

/// The outcome of one specialized tool handler invocation.
pub enum LuaExecution {
    Computed {
        response: Value,
        state_patch: Option<Map<String, Value>>,
        workspace: Workspace,
        operations: Vec<WorkspaceOp>,
    },
    Fallback {
        reason: String,
        operations: Vec<WorkspaceOp>,
    },
    Failed {
        error: String,
        operations: Vec<WorkspaceOp>,
    },
}

/// Generate a syntactically valid module that declines every named tool.
pub fn fallback_source(tools: &[ToolSchema]) -> String {
    let mut out = String::from("return {\n");
    for tool in tools {
        out.push_str("  [");
        out.push_str(&lua_quote(&tool.name));
        out.push_str(
            "] = function(args, ctx) return PleaseSimulateException(\"not specialized\") end,\n",
        );
    }
    out.push_str("}\n");
    out
}

fn lua_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for ch in value.chars() {
        match ch {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            '\u{08}' => quoted.push_str("\\008"),
            '\u{0C}' => quoted.push_str("\\012"),
            // Lua decimal escapes encode bytes, not Unicode scalar values.
            // Escape only ASCII controls; C1 controls remain their UTF-8 bytes.
            c if (c as u32) <= 0x1f || c == '\u{7f}' => {
                use fmt::Write;
                write!(quoted, "\\{:03}", c as u32).expect("writing a String cannot fail");
            }
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

/// Execute in a fresh VM. Workspace mutations are returned only on a valid
/// computed result; failures and fallbacks retain accurate, bounded operations
/// but never mutate the caller's workspace.
pub fn execute(
    source: &str,
    tool: &ToolSchema,
    call: &ToolCall,
    world_state: &Map<String, Value>,
    workspace: &Workspace,
    options: &LuaOptions,
) -> LuaExecution {
    if let Err(error) = validate_options(options) {
        return LuaExecution::Failed {
            error: clip(&error, options.max_result_bytes),
            operations: Vec::new(),
        };
    }
    if source.len() > options.max_source_bytes {
        return LuaExecution::Failed {
            error: clip(
                &format!(
                    "Lua source is {} bytes; limit is {}",
                    source.len(),
                    options.max_source_bytes
                ),
                options.max_result_bytes,
            ),
            operations: Vec::new(),
        };
    }

    let state = Rc::new(RefCell::new(HostState {
        workspace: workspace.clone(),
        operations: Vec::new(),
        calls: 0,
        bytes: 0,
    }));
    let result = run(source, tool, call, world_state, options, state.clone());
    let host = state.borrow();
    match result {
        Ok((response, state_patch)) => LuaExecution::Computed {
            response,
            state_patch,
            workspace: host.workspace.clone(),
            operations: host.operations.clone(),
        },
        Err(RunError::Fallback(reason)) => LuaExecution::Fallback {
            reason: clip(&reason, options.max_result_bytes),
            operations: host.operations.clone(),
        },
        Err(RunError::Failed(error)) => LuaExecution::Failed {
            error: clip(&error, options.max_result_bytes),
            operations: host.operations.clone(),
        },
    }
}

impl LuaOptions {
    /// Validate limits for both API and standalone callers. No zero means
    /// unlimited, and callers cannot disable host stack protection.
    pub fn validate(&self) -> Result<(), String> {
        validate_options(self)
    }
}

fn validate_options(options: &LuaOptions) -> Result<(), String> {
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

fn clip(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    if max_bytes == 0 {
        return String::new();
    }
    // Use an ASCII marker only when it fits; diagnostics must honor the same
    // bound even for invalid zero/tiny direct-library option values.
    let (payload, suffix) = if max_bytes >= 3 {
        (max_bytes - 3, "...")
    } else {
        (max_bytes, "")
    };
    let end = value
        .char_indices()
        .take_while(|(index, ch)| *index + ch.len_utf8() <= payload)
        .map(|(index, ch)| index + ch.len_utf8())
        .last()
        .unwrap_or(0);
    format!("{}{}", &value[..end], suffix)
}

struct HostState {
    workspace: Workspace,
    operations: Vec<WorkspaceOp>,
    calls: usize,
    bytes: usize,
}

#[derive(Debug)]
struct FallbackSentinel(String);
impl fmt::Display for FallbackSentinel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl StdError for FallbackSentinel {}

enum RunError {
    Fallback(String),
    Failed(String),
}

/// Shared by Lua instruction hooks and all conversion loops. Charging
/// conversion work prevents a small, aliased Lua graph from expanding into an
/// unbounded amount of Rust work after the handler has returned.
#[derive(Clone)]
struct Budget {
    deadline: Instant,
    remaining: Rc<RefCell<u64>>,
}

impl Budget {
    fn new(options: &LuaOptions) -> Self {
        Self {
            deadline: Instant::now() + std::time::Duration::from_millis(options.max_duration_ms),
            remaining: Rc::new(RefCell::new(options.max_instructions)),
        }
    }

    fn charge(&self) -> mlua::Result<()> {
        if Instant::now() >= self.deadline {
            return Err(LuaError::RuntimeError(
                "Lua execution exceeded duration limit".into(),
            ));
        }
        let mut remaining = self.remaining.borrow_mut();
        if *remaining == 0 {
            return Err(LuaError::RuntimeError(
                "Lua execution exceeded instruction/conversion limit".into(),
            ));
        }
        *remaining -= 1;
        Ok(())
    }
}

fn run(
    source: &str,
    tool: &ToolSchema,
    call: &ToolCall,
    world_state: &Map<String, Value>,
    options: &LuaOptions,
    host: Rc<RefCell<HostState>>,
) -> Result<(Value, Option<Map<String, Value>>), RunError> {
    let budget = Budget::new(options);
    budget.charge().map_err(classify_lua_error)?;
    let lua = Lua::new_with(StdLib::ALL_SAFE, MluaOptions::default())
        .map_err(|e| RunError::Failed(format!("could not create Lua VM: {e}")))?;
    lua.set_memory_limit(options.max_memory_bytes)
        .map_err(|e| RunError::Failed(format!("could not set Lua memory limit: {e}")))?;
    install_budget_hook(&lua, budget.clone());
    let json = install_sandbox(&lua)?;

    budget.charge().map_err(classify_lua_error)?;
    let handlers: Table = lua
        .load(source)
        .set_mode(ChunkMode::Text)
        .eval()
        .map_err(classify_lua_error)?;
    let handler: LuaValue = handlers
        .get(tool.name.as_str())
        .map_err(classify_lua_error)?;
    let function = match handler {
        LuaValue::Nil => return Err(RunError::Fallback("no specialized handler".into())),
        LuaValue::Function(function) => function,
        _ => {
            return Err(RunError::Failed(
                "selected Lua handler is not a function".into(),
            ));
        }
    };

    let input_limits = JsonLimits::new(options, budget.clone());
    let args =
        json_to_lua(&lua, &call.args, &json, &input_limits, 0).map_err(classify_lua_error)?;
    let ctx = lua.create_table().map_err(classify_lua_error)?;
    let world =
        json_map_to_lua(&lua, world_state, &json, &input_limits, 0).map_err(classify_lua_error)?;
    ctx.set("world_state", world).map_err(classify_lua_error)?;
    ctx.set(
        "workspace",
        workspace_capability(&lua, host, &json, options, budget.clone())?,
    )
    .map_err(classify_lua_error)?;

    budget.charge().map_err(classify_lua_error)?;
    let result: LuaValue = function.call((args, ctx)).map_err(classify_lua_error)?;
    budget.charge().map_err(classify_lua_error)?;
    let result = match result {
        LuaValue::Nil => {
            return Err(RunError::Failed(
                "Lua handler returned nil; use json.null for a null response".into(),
            ));
        }
        LuaValue::Table(table) => table,
        _ => {
            return Err(RunError::Failed(
                "Lua handler result must be a table".into(),
            ));
        }
    };
    let response: LuaValue = result.get("response").map_err(classify_lua_error)?;
    if matches!(response, LuaValue::Nil) {
        return Err(RunError::Failed(
            "Lua handler result is missing response".into(),
        ));
    }
    let output_limits = JsonLimits::new(options, budget.clone());
    let response = lua_to_json(response, &json, &output_limits, 0).map_err(classify_lua_error)?;

    let patch: LuaValue = result.get("state_patch").map_err(classify_lua_error)?;
    match tool.side_effect {
        SideEffect::Read => {
            if !matches!(patch, LuaValue::Nil) {
                let patch =
                    lua_to_json(patch, &json, &output_limits, 0).map_err(classify_lua_error)?;
                match patch {
                    Value::Object(object) if object.is_empty() => {}
                    _ => {
                        return Err(RunError::Failed(
                            "read tool returned a nonempty state_patch".into(),
                        ));
                    }
                }
            }
            output_limits.finish().map_err(classify_lua_error)?;
            Ok((response, None))
        }
        SideEffect::Write => {
            if matches!(patch, LuaValue::Nil) {
                return Err(RunError::Failed(
                    "write tool result is missing state_patch".into(),
                ));
            }
            let patch = lua_to_json(patch, &json, &output_limits, 0).map_err(classify_lua_error)?;
            output_limits.finish().map_err(classify_lua_error)?;
            match patch {
                Value::Object(object) => Ok((response, Some(object))),
                _ => Err(RunError::Failed(
                    "write tool state_patch must be an object".into(),
                )),
            }
        }
    }
}

fn install_budget_hook(lua: &Lua, budget: Budget) {
    lua.set_hook(HookTriggers::new().every_nth_instruction(1), move |_, _| {
        budget.charge()?;
        Ok(VmState::Continue)
    });
}

/// Counts exact JSON bytes while conversion constructs a value. This is a
/// single budget shared by response and state patch, so their combined result
/// cannot exceed `max_result_bytes`.
#[derive(Clone)]
struct JsonLimits {
    max_bytes: usize,
    bytes: Rc<RefCell<usize>>,
    max_depth: usize,
    budget: Budget,
}

impl JsonLimits {
    fn new(options: &LuaOptions, budget: Budget) -> Self {
        Self {
            max_bytes: options.max_result_bytes,
            bytes: Rc::new(RefCell::new(0)),
            max_depth: options.max_value_depth,
            budget,
        }
    }

    fn enter(&self, depth: usize) -> mlua::Result<()> {
        self.budget.charge()?;
        if depth > self.max_depth {
            return Err(LuaError::RuntimeError(format!(
                "JSON value depth exceeds {}",
                self.max_depth
            )));
        }
        Ok(())
    }

    fn add(&self, bytes: usize) -> mlua::Result<()> {
        self.budget.charge()?;
        let mut used = self.bytes.borrow_mut();
        if bytes > self.max_bytes.saturating_sub(*used) {
            return Err(LuaError::RuntimeError(format!(
                "JSON value exceeds {}-byte limit",
                self.max_bytes
            )));
        }
        *used += bytes;
        Ok(())
    }

    fn finish(&self) -> mlua::Result<()> {
        self.budget.charge()
    }
}

struct JsonBridge {
    null: Table,
    array_metatable: Table,
}

fn install_sandbox(lua: &Lua) -> Result<JsonBridge, RunError> {
    let globals = lua.globals();
    // Load ordinary base/table/string/math helpers, then remove all loading,
    // host-capability, randomness, and error-catching escape hatches before
    // any untrusted source runs.
    for name in [
        "io",
        "os",
        "package",
        "debug",
        "require",
        "load",
        "loadfile",
        "dofile",
        "pcall",
        "xpcall",
        "coroutine",
        "collectgarbage",
        "print",
        "warn",
        "rawset",
        "setmetatable",
        // Do not expose the shared json.array metatable (or string metatable):
        // guest-installed __gc/__index hooks could outlive normal execution.
        "getmetatable",
        "utf8",
    ] {
        globals
            .set(name, LuaValue::Nil)
            .map_err(classify_lua_error)?;
    }
    let string: Table = globals.get("string").map_err(classify_lua_error)?;
    string
        .set("dump", LuaValue::Nil)
        .map_err(classify_lua_error)?;
    let math: Table = globals.get("math").map_err(classify_lua_error)?;
    math.set("random", LuaValue::Nil)
        .map_err(classify_lua_error)?;
    math.set("randomseed", LuaValue::Nil)
        .map_err(classify_lua_error)?;

    let null = lua.create_table().map_err(classify_lua_error)?;
    seal_metatable(lua, &null, true)?;
    let array_metatable = lua.create_table().map_err(classify_lua_error)?;
    seal_metatable(lua, &array_metatable, false)?;
    let null_for_array = null.clone();
    let array_meta = array_metatable.clone();
    let array = lua
        .create_function(move |_, table: Table| {
            if table == null_for_array {
                return Err(LuaError::RuntimeError(
                    "json.null cannot be made into an array".into(),
                ));
            }
            table.set_metatable(Some(array_meta.clone()));
            Ok(table)
        })
        .map_err(classify_lua_error)?;
    let json = lua.create_table().map_err(classify_lua_error)?;
    json.set("null", null.clone()).map_err(classify_lua_error)?;
    json.set("array", array).map_err(classify_lua_error)?;
    globals.set("json", json).map_err(classify_lua_error)?;

    let decline = lua
        .create_function(|_, reason: Option<String>| -> mlua::Result<()> {
            Err(LuaError::external(FallbackSentinel(
                reason.unwrap_or_else(|| "not specialized".into()),
            )))
        })
        .map_err(classify_lua_error)?;
    globals
        .set("PleaseSimulateException", decline)
        .map_err(classify_lua_error)?;
    Ok(JsonBridge {
        null,
        array_metatable,
    })
}

fn seal_metatable(lua: &Lua, table: &Table, reject_writes: bool) -> Result<(), RunError> {
    let metatable = lua.create_table().map_err(classify_lua_error)?;
    metatable
        .set("__metatable", false)
        .map_err(classify_lua_error)?;
    if reject_writes {
        let reject = lua
            .create_function(|_, _: (LuaValue, LuaValue, LuaValue)| -> mlua::Result<()> {
                Err(LuaError::RuntimeError("json.null is immutable".into()))
            })
            .map_err(classify_lua_error)?;
        metatable
            .set("__newindex", reject)
            .map_err(classify_lua_error)?;
    }
    table.set_metatable(Some(metatable));
    Ok(())
}

fn workspace_capability(
    lua: &Lua,
    host: Rc<RefCell<HostState>>,
    json: &JsonBridge,
    options: &LuaOptions,
    budget: Budget,
) -> Result<Table, RunError> {
    let capability = lua.create_table().map_err(classify_lua_error)?;
    for capability_name in ["read", "write", "list_dir", "grep"] {
        let host = host.clone();
        let json = JsonBridge {
            null: json.null.clone(),
            array_metatable: json.array_metatable.clone(),
        };
        let name = capability_name.to_owned();
        let conversion_options = options.clone();
        let budget = budget.clone();
        let function = lua
            .create_function(move |lua, args: LuaValue| {
                budget.charge()?;
                let args = match args {
                    LuaValue::Table(_) => {
                        let limits = JsonLimits::new(&conversion_options, budget.clone());
                        let args = lua_to_json(args, &json, &limits, 0)?;
                        limits.finish()?;
                        args
                    }
                    _ => {
                        return Err(LuaError::RuntimeError(
                            "workspace capability expects one JSON-like args table".into(),
                        ));
                    }
                };
                let arg_bytes = bounded_serialized_len(&args, conversion_options.max_result_bytes)?;
                // Check after argument conversion and immediately before the
                // host operation. `Workspace::exec` is deterministic, but it
                // can still process a bounded, nontrivial workspace result.
                budget.charge()?;
                let result = {
                    let mut state = host.borrow_mut();
                    if state.calls >= conversion_options.max_host_calls {
                        return Err(LuaError::RuntimeError(
                            "workspace capability call limit exceeded".into(),
                        ));
                    }
                    if arg_bytes
                        > conversion_options
                            .max_host_bytes
                            .saturating_sub(state.bytes)
                    {
                        return Err(LuaError::RuntimeError(
                            "workspace capability byte limit exceeded".into(),
                        ));
                    }
                    state.calls += 1;
                    let remaining_host_bytes = conversion_options
                        .max_host_bytes
                        .saturating_sub(state.bytes + arg_bytes);
                    let result = state.workspace.exec_bounded(
                        &name,
                        &args,
                        conversion_options
                            .max_result_bytes
                            .min(remaining_host_bytes),
                    );
                    budget.charge()?;
                    let result_bytes =
                        bounded_serialized_len(&result, conversion_options.max_result_bytes)?;
                    budget.charge()?;
                    if result_bytes
                        > conversion_options
                            .max_host_bytes
                            .saturating_sub(state.bytes + arg_bytes)
                    {
                        return Err(LuaError::RuntimeError(
                            "workspace capability byte limit exceeded".into(),
                        ));
                    }
                    state.bytes += arg_bytes + result_bytes;
                    state.operations.push(WorkspaceOp {
                        tool: name.clone(),
                        args: args.clone(),
                        result: result.clone(),
                    });
                    result
                };
                let limits = JsonLimits::new(&conversion_options, budget.clone());
                let result = json_to_lua(lua, &result, &json, &limits, 0)?;
                limits.finish()?;
                budget.charge()?;
                Ok(result)
            })
            .map_err(classify_lua_error)?;
        capability
            .set(capability_name, function)
            .map_err(classify_lua_error)?;
    }
    Ok(capability)
}

fn bounded_serialized_len(value: &Value, max: usize) -> mlua::Result<usize> {
    let size = json_value_size(value, 0, MAX_VALUE_DEPTH)?;
    if size > max {
        return Err(LuaError::RuntimeError(format!(
            "JSON value exceeds {max}-byte limit"
        )));
    }
    Ok(size)
}

fn json_string_size(value: &str) -> usize {
    2 + value
        .bytes()
        .map(|byte| match byte {
            b'"' | b'\\' | b'\n' | b'\r' | b'\t' | 0x08 | 0x0c => 2,
            0x00..=0x1f => 6,
            _ => 1,
        })
        .sum::<usize>()
}

fn json_value_size(value: &Value, depth: usize, max_depth: usize) -> mlua::Result<usize> {
    if depth > max_depth {
        return Err(LuaError::RuntimeError(
            "JSON value is too deeply nested to serialize".into(),
        ));
    }
    Ok(match value {
        Value::Null => 4,
        Value::Bool(true) => 4,
        Value::Bool(false) => 5,
        Value::Number(n) => n.to_string().len(),
        Value::String(s) => json_string_size(s),
        Value::Array(values) => {
            values
                .iter()
                .enumerate()
                .try_fold(2usize, |size, (i, value)| {
                    let comma = usize::from(i != 0);
                    Ok::<_, LuaError>(size.saturating_add(comma).saturating_add(json_value_size(
                        value,
                        depth + 1,
                        max_depth,
                    )?))
                })?
        }
        Value::Object(values) => {
            values
                .iter()
                .enumerate()
                .try_fold(2usize, |size, (i, (key, value))| {
                    let comma = usize::from(i != 0);
                    Ok::<_, LuaError>(
                        size.saturating_add(comma)
                            .saturating_add(json_string_size(key))
                            .saturating_add(1)
                            .saturating_add(json_value_size(value, depth + 1, max_depth)?),
                    )
                })?
        }
    })
}

fn json_to_lua(
    lua: &Lua,
    value: &Value,
    json: &JsonBridge,
    limits: &JsonLimits,
    depth: usize,
) -> mlua::Result<LuaValue> {
    limits.enter(depth)?;
    Ok(match value {
        Value::Null => {
            limits.add(4)?;
            LuaValue::Table(json.null.clone())
        }
        Value::Bool(value) => {
            limits.add(if *value { 4 } else { 5 })?;
            LuaValue::Boolean(*value)
        }
        Value::String(value) => {
            limits.add(json_string_size(value))?;
            LuaValue::String(lua.create_string(value)?)
        }
        Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                limits.add(value.to_string().len())?;
                LuaValue::Integer(value)
            } else if value.as_u64().is_some() {
                return Err(LuaError::RuntimeError(
                    "JSON unsigned integer exceeds Lua's exact signed-integer range".into(),
                ));
            } else {
                let value = value
                    .as_f64()
                    .ok_or_else(|| LuaError::RuntimeError("invalid JSON number".into()))?;
                let number = serde_json::Number::from_f64(value)
                    .ok_or_else(|| LuaError::RuntimeError("non-finite JSON number".into()))?;
                limits.add(number.to_string().len())?;
                LuaValue::Number(value)
            }
        }
        Value::Array(values) => {
            limits.add(1)?;
            let table = lua.create_table()?;
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    limits.add(1)?;
                }
                table.raw_set(index + 1, json_to_lua(lua, value, json, limits, depth + 1)?)?;
            }
            limits.add(1)?;
            table.set_metatable(Some(json.array_metatable.clone()));
            LuaValue::Table(table)
        }
        Value::Object(values) => json_map_to_lua(lua, values, json, limits, depth)?,
    })
}

fn json_map_to_lua(
    lua: &Lua,
    values: &Map<String, Value>,
    json: &JsonBridge,
    limits: &JsonLimits,
    depth: usize,
) -> mlua::Result<LuaValue> {
    limits.enter(depth)?;
    limits.add(1)?;
    let table = lua.create_table()?;
    for (index, (key, value)) in values.iter().enumerate() {
        if index != 0 {
            limits.add(1)?;
        }
        limits.add(json_string_size(key) + 1)?;
        table.raw_set(
            key.as_str(),
            json_to_lua(lua, value, json, limits, depth + 1)?,
        )?;
    }
    limits.add(1)?;
    Ok(LuaValue::Table(table))
}

fn lua_to_json(
    value: LuaValue,
    json: &JsonBridge,
    limits: &JsonLimits,
    depth: usize,
) -> mlua::Result<Value> {
    let mut seen = HashSet::new();
    lua_to_json_inner(value, json, limits, depth, &mut seen)
}

fn lua_to_json_inner(
    value: LuaValue,
    json: &JsonBridge,
    limits: &JsonLimits,
    depth: usize,
    seen: &mut HashSet<usize>,
) -> mlua::Result<Value> {
    limits.enter(depth)?;
    match value {
        LuaValue::Nil => Err(LuaError::RuntimeError(
            "nil is not JSON; use json.null".into(),
        )),
        LuaValue::Boolean(value) => {
            limits.add(if value { 4 } else { 5 })?;
            Ok(Value::Bool(value))
        }
        LuaValue::Integer(value) => {
            limits.add(value.to_string().len())?;
            Ok(Value::Number(value.into()))
        }
        LuaValue::Number(value) => {
            let number = serde_json::Number::from_f64(value).ok_or_else(|| {
                LuaError::RuntimeError("non-finite Lua number is not JSON".into())
            })?;
            limits.add(number.to_string().len())?;
            Ok(Value::Number(number))
        }
        LuaValue::String(value) => {
            let value = value.to_str()?;
            // Charge before cloning bytes out of the Lua VM: a hostile Lua
            // string must not allocate an unbounded Rust String first.
            limits.add(json_string_size(&value))?;
            Ok(Value::String(value.to_owned()))
        }
        LuaValue::Table(table) => {
            if table == json.null {
                limits.add(4)?;
                return Ok(Value::Null);
            }
            let pointer = table.to_pointer() as usize;
            if !seen.insert(pointer) {
                return Err(LuaError::RuntimeError(
                    "cyclic Lua tables are not JSON".into(),
                ));
            }
            let result = if table.metatable().as_ref() == Some(&json.array_metatable) {
                let len = table.raw_len();
                // At least `[]` plus one byte for every element; reject before
                // Vec allocation or iteration when raw_len is hostile.
                if len
                    > limits
                        .max_bytes
                        .saturating_sub(*limits.bytes.borrow())
                        .saturating_sub(2)
                {
                    return Err(LuaError::RuntimeError("json.array is too large".into()));
                }
                limits.add(1)?;
                let mut array = Vec::new();
                for index in 1..=len {
                    if index != 1 {
                        limits.add(1)?;
                    }
                    array.push(lua_to_json_inner(
                        table.raw_get(index)?,
                        json,
                        limits,
                        depth + 1,
                        seen,
                    )?);
                }
                limits.add(1)?;
                for pair in table.clone().pairs::<LuaValue, LuaValue>() {
                    limits.budget.charge()?;
                    let (key, _) = pair?;
                    if !matches!(key, LuaValue::Integer(i) if i >= 1 && (i as usize) <= len) {
                        return Err(LuaError::RuntimeError(
                            "json.array contains a non-array key".into(),
                        ));
                    }
                }
                Value::Array(array)
            } else {
                let mut integers = Vec::new();
                let mut object = Map::new();
                let mut has_strings = false;
                for pair in table.clone().pairs::<LuaValue, LuaValue>() {
                    limits.budget.charge()?;
                    let (key, value) = pair?;
                    match key {
                        LuaValue::String(key) => {
                            has_strings = true;
                            let key = key.to_str()?;
                            // As above, size-check before making an owned key.
                            limits.add(json_string_size(&key) + 1)?;
                            let key = key.to_owned();
                            if !object.is_empty() {
                                limits.add(1)?;
                            }
                            object.insert(
                                key,
                                lua_to_json_inner(value, json, limits, depth + 1, seen)?,
                            );
                        }
                        LuaValue::Integer(key) if key >= 1 => {
                            // Each eventual array member needs at least one
                            // byte. Bound the temporary vector before it grows.
                            if integers.len() >= limits.max_bytes {
                                return Err(LuaError::RuntimeError(
                                    "JSON array is too large".into(),
                                ));
                            }
                            integers.push((key as usize, value));
                        }
                        _ => {
                            return Err(LuaError::RuntimeError(
                                "JSON object keys must be strings".into(),
                            ));
                        }
                    }
                }
                if has_strings && !integers.is_empty() {
                    return Err(LuaError::RuntimeError(
                        "JSON table cannot mix object and array keys".into(),
                    ));
                }
                if has_strings || integers.is_empty() {
                    limits.add(2)?;
                    Value::Object(object)
                } else {
                    integers.sort_by_key(|(key, _)| *key);
                    limits.add(1)?;
                    let mut array = Vec::new();
                    for (expected, (key, value)) in integers.into_iter().enumerate() {
                        if key != expected + 1 {
                            return Err(LuaError::RuntimeError(
                                "JSON arrays must have consecutive 1-based indexes".into(),
                            ));
                        }
                        if expected != 0 {
                            limits.add(1)?;
                        }
                        array.push(lua_to_json_inner(value, json, limits, depth + 1, seen)?);
                    }
                    limits.add(1)?;
                    Value::Array(array)
                }
            };
            seen.remove(&pointer);
            Ok(result)
        }
        _ => Err(LuaError::RuntimeError(
            "Lua JSON values may only contain strings, numbers, booleans, and tables".into(),
        )),
    }
}

fn classify_lua_error(error: LuaError) -> RunError {
    for value in error.chain() {
        if let Some(sentinel) = value.downcast_ref::<FallbackSentinel>() {
            return RunError::Fallback(sentinel.0.clone());
        }
    }
    RunError::Failed(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, side_effect: SideEffect) -> ToolSchema {
        ToolSchema {
            name: name.into(),
            description: String::new(),
            parameters: Value::Object(Map::new()),
            side_effect,
            example_responses: vec![],
        }
    }
    fn call(args: Value) -> ToolCall {
        ToolCall {
            name: "x".into(),
            args,
        }
    }
    fn computed(
        value: LuaExecution,
    ) -> (
        Value,
        Option<Map<String, Value>>,
        Workspace,
        Vec<WorkspaceOp>,
    ) {
        match value {
            LuaExecution::Computed {
                response,
                state_patch,
                workspace,
                operations,
            } => (response, state_patch, workspace, operations),
            _ => panic!("expected computed"),
        }
    }
    fn failed(value: LuaExecution) -> String {
        match value {
            LuaExecution::Failed { error, .. } => error,
            _ => panic!("expected failed"),
        }
    }

    #[test]
    fn diagnostic_clipping_and_host_stack_ceiling_are_strict() {
        for max in 0..16 {
            assert!(clip("雪雪雪雪雪雪雪雪", max).len() <= max);
        }
        let options = LuaOptions {
            max_value_depth: MAX_VALUE_DEPTH + 1,
            ..Default::default()
        };
        assert!(options.validate().unwrap_err().contains("max_value_depth"));

        for (name, options) in [
            (
                "max_memory_bytes",
                LuaOptions {
                    max_memory_bytes: MAX_MEMORY_BYTES + 1,
                    ..Default::default()
                },
            ),
            (
                "max_instructions",
                LuaOptions {
                    max_instructions: MAX_INSTRUCTIONS + 1,
                    ..Default::default()
                },
            ),
            (
                "max_host_calls",
                LuaOptions {
                    max_host_calls: MAX_HOST_CALLS + 1,
                    ..Default::default()
                },
            ),
            (
                "max_host_bytes",
                LuaOptions {
                    max_host_bytes: MAX_HOST_BYTES + 1,
                    ..Default::default()
                },
            ),
            (
                "max_source_bytes",
                LuaOptions {
                    max_source_bytes: MAX_SOURCE_BYTES + 1,
                    ..Default::default()
                },
            ),
            (
                "max_duration_ms",
                LuaOptions {
                    max_duration_ms: MAX_DURATION_MS + 1,
                    ..Default::default()
                },
            ),
            (
                "max_result_bytes",
                LuaOptions {
                    max_result_bytes: MAX_RESULT_BYTES + 1,
                    ..Default::default()
                },
            ),
        ] {
            let error = options.validate().unwrap_err();
            assert!(error.contains(name), "{error}");
            assert!(error.contains("must not exceed"), "{error}");
        }
    }

    #[test]
    fn memory_bytecode_cycles_and_rollback_limits_are_tested() {
        let t = tool("x", SideEffect::Read);
        let original = Workspace::empty();
        let memory = LuaOptions {
            max_memory_bytes: 128 * 1024,
            ..Default::default()
        };
        let error = failed(execute(
            "return {x=function() return {response=string.rep('x',1000000)} end}",
            &t,
            &call(Value::Null),
            &Map::new(),
            &original,
            &memory,
        ));
        assert!(error.to_lowercase().contains("memory"));
        for source in [
            "\x1bLua",
            "return {x=function() local t={}; t.self=t; return {response=t} end}",
        ] {
            assert!(matches!(
                execute(
                    source,
                    &t,
                    &call(Value::Null),
                    &Map::new(),
                    &original,
                    &LuaOptions::default()
                ),
                LuaExecution::Failed { .. }
            ));
        }
        let source = "return {x=function(a,c) c.workspace.write({path='x',content='staged'}); c.workspace.write({path='y',content='staged'}); return {response=true} end}";
        let limits = LuaOptions {
            max_host_calls: 1,
            ..Default::default()
        };
        let result = execute(
            source,
            &t,
            &call(Value::Null),
            &Map::new(),
            &original,
            &limits,
        );
        assert!(matches!(result, LuaExecution::Failed { operations, .. } if operations.len()==1));
        assert!(original.file_bytes("x").is_none());
        let limits = LuaOptions {
            max_host_bytes: 1,
            ..Default::default()
        };
        assert!(matches!(
            execute(
                source,
                &t,
                &call(Value::Null),
                &Map::new(),
                &original,
                &limits
            ),
            LuaExecution::Failed { .. }
        ));
        let source = "return {x=function() return {response=(getmetatable==nil and setmetatable==nil and pcall==nil)} end}";
        assert_eq!(
            computed(execute(
                source,
                &t,
                &call(Value::Null),
                &Map::new(),
                &original,
                &LuaOptions::default()
            ))
            .0,
            Value::Bool(true)
        );
    }

    #[test]
    fn weird_tool_names_quote_as_exact_utf8() {
        let names = [
            "quote\" slash\\ nul\0 newline\n",
            "c1-\u{0080}\u{009f}-snow-雪",
            "del-\u{007f}",
        ];
        let tools: Vec<_> = names
            .iter()
            .map(|name| tool(name, SideEffect::Read))
            .collect();
        let source = fallback_source(&tools);
        for tool in &tools {
            assert!(
                matches!(execute(&source, tool, &call(Value::Null), &Map::new(), &Workspace::empty(), &LuaOptions::default()), LuaExecution::Fallback { reason, .. } if reason == "not specialized")
            );
        }
    }

    #[test]
    fn typed_fallback_is_not_an_error_message_substring() {
        let t = tool("x", SideEffect::Read);
        assert!(matches!(
            execute(
                "return {x=function() PleaseSimulateException('no') end}",
                &t,
                &call(Value::Null),
                &Map::new(),
                &Workspace::empty(),
                &LuaOptions::default()
            ),
            LuaExecution::Fallback { .. }
        ));
        assert!(matches!(
            execute(
                "return {x=function() error('PleaseSimulateException') end}",
                &t,
                &call(Value::Null),
                &Map::new(),
                &Workspace::empty(),
                &LuaOptions::default()
            ),
            LuaExecution::Failed { .. }
        ));
        assert!(matches!(
            execute(
                "return {}",
                &t,
                &call(Value::Null),
                &Map::new(),
                &Workspace::empty(),
                &LuaOptions::default()
            ),
            LuaExecution::Fallback { .. }
        ));
    }

    #[test]
    fn state_and_workspace_commit_only_on_valid_compute() {
        let write = tool("x", SideEffect::Write);
        let original = Workspace::empty();
        let source = "return {x=function(a,c) c.workspace.write({path='x',content='bad'}); PleaseSimulateException('no') end}";
        let outcome = execute(
            source,
            &write,
            &call(Value::Null),
            &Map::new(),
            &original,
            &LuaOptions::default(),
        );
        assert!(
            matches!(outcome, LuaExecution::Fallback { operations, .. } if operations.len() == 1)
        );
        assert_eq!(
            original
                .clone()
                .exec("read", &serde_json::json!({"path":"x"}))["error"],
            "not found"
        );
        let source = "return {x=function(a,c) c.workspace.write({path='x',content='ok'}); return {response=json.null,state_patch={a=1}} end}";
        let (response, patch, mut workspace, _) = computed(execute(
            source,
            &write,
            &call(Value::Null),
            &Map::new(),
            &original,
            &LuaOptions::default(),
        ));
        assert_eq!(response, Value::Null);
        assert_eq!(patch.unwrap()["a"], 1);
        assert_eq!(
            workspace.exec("read", &serde_json::json!({"path":"x"}))["content"],
            "ok"
        );
    }

    #[test]
    fn json_null_empty_collections_unicode_and_integers_are_safe() {
        let t = tool("x", SideEffect::Read);
        let source = "return {x=function(a) return {response={null=json.null,array=json.array({}),object={},word='雪',n=a.n}} end}";
        assert_eq!(
            computed(execute(
                source,
                &t,
                &call(serde_json::json!({"n":9223372036854775807i64})),
                &Map::new(),
                &Workspace::empty(),
                &LuaOptions::default()
            ))
            .0,
            serde_json::json!({"null":null,"array":[],"object":{},"word":"雪","n":9223372036854775807i64})
        );
        let error = failed(execute(
            "return {x=function() return {response=true} end}",
            &t,
            &call(serde_json::json!({"n":9223372036854775808u64})),
            &Map::new(),
            &Workspace::empty(),
            &LuaOptions::default(),
        ));
        assert!(error.contains("unsigned integer"));
    }

    #[test]
    fn sandbox_and_null_are_not_mutable_escape_hatches() {
        let t = tool("x", SideEffect::Read);
        let source = "return {x=function() return {response=(io==nil and os==nil and package==nil and debug==nil and require==nil and load==nil and pcall==nil and xpcall==nil and coroutine==nil and string.dump==nil and math.random==nil)} end}";
        assert_eq!(
            computed(execute(
                source,
                &t,
                &call(Value::Null),
                &Map::new(),
                &Workspace::empty(),
                &LuaOptions::default()
            ))
            .0,
            Value::Bool(true)
        );
        assert!(matches!(
            execute(
                "return {x=function() json.array(json.null); return {response=true} end}",
                &t,
                &call(Value::Null),
                &Map::new(),
                &Workspace::empty(),
                &LuaOptions::default()
            ),
            LuaExecution::Failed { .. }
        ));
        assert!(matches!(
            execute(
                "return {x=function() json.null.x=1; return {response=true} end}",
                &t,
                &call(Value::Null),
                &Map::new(),
                &Workspace::empty(),
                &LuaOptions::default()
            ),
            LuaExecution::Failed { .. }
        ));
    }

    #[test]
    fn source_memory_instruction_and_zero_limits_fail() {
        let t = tool("x", SideEffect::Read);
        let mut source_limit = LuaOptions::default();
        source_limit.max_source_bytes = 1;
        assert!(matches!(
            execute(
                "return {}",
                &t,
                &call(Value::Null),
                &Map::new(),
                &Workspace::empty(),
                &source_limit
            ),
            LuaExecution::Failed { .. }
        ));
        let mut instructions = LuaOptions::default();
        instructions.max_instructions = 100;
        for source in [
            "while true do end; return {}",
            "return {x=function() while true do end end}",
        ] {
            assert!(matches!(
                execute(
                    source,
                    &t,
                    &call(Value::Null),
                    &Map::new(),
                    &Workspace::empty(),
                    &instructions
                ),
                LuaExecution::Failed { .. }
            ));
        }
        for set_zero in [0usize, 1, 2, 3, 4, 5, 6, 7] {
            let mut options = LuaOptions::default();
            match set_zero {
                0 => options.max_memory_bytes = 0,
                1 => options.max_instructions = 0,
                2 => options.max_host_calls = 0,
                3 => options.max_host_bytes = 0,
                4 => options.max_source_bytes = 0,
                5 => options.max_duration_ms = 0,
                6 => options.max_value_depth = 0,
                _ => options.max_result_bytes = 0,
            }
            assert!(matches!(
                execute(
                    "return {}",
                    &t,
                    &call(Value::Null),
                    &Map::new(),
                    &Workspace::empty(),
                    &options
                ),
                LuaExecution::Failed { .. }
            ));
        }
    }

    #[test]
    fn depth_aliases_sparse_arrays_and_large_results_are_bounded() {
        let t = tool("x", SideEffect::Read);
        let mut depth = LuaOptions::default();
        depth.max_value_depth = 8;
        assert!(matches!(
            execute(
                "return {x=function() local t={}; local p=t; for i=1,20 do local n={}; p.a=n; p=n end; return {response=t} end}",
                &t,
                &call(Value::Null),
                &Map::new(),
                &Workspace::empty(),
                &depth
            ),
            LuaExecution::Failed { .. }
        ));
        let mut size = LuaOptions::default();
        size.max_result_bytes = 64;
        for source in [
            "return {x=function() return {response=string.rep('x', 1000)} end}",
            "return {x=function() return {response={[1000000000]=true}} end}",
            "return {x=function() local a={x='1234567890'}; return {response={a,a,a,a,a,a,a,a,a,a,a,a}} end}",
            "return {x=function() return {response=json.array({[1000000000]=true})} end}",
        ] {
            assert!(
                matches!(
                    execute(
                        source,
                        &t,
                        &call(Value::Null),
                        &Map::new(),
                        &Workspace::empty(),
                        &size
                    ),
                    LuaExecution::Failed { .. }
                ),
                "{source}"
            );
        }
    }

    #[test]
    fn capability_limits_and_existing_workspace_tools_work() {
        let t = tool("x", SideEffect::Read);
        let mut workspace = Workspace::empty();
        workspace.exec(
            "write",
            &serde_json::json!({"path":"src/a.txt","content":"alpha\nbeta"}),
        );
        let source = "return {x=function(a,c) local l=c.workspace.list_dir({path='src'}); local r=c.workspace.read({path='src/a.txt'}); local g=c.workspace.grep({pattern='beta'}); return {response={n=#l.entries,text=r.content,hits=#g.matches}} end}";
        let (response, _, _, operations) = computed(execute(
            source,
            &t,
            &call(Value::Null),
            &Map::new(),
            &workspace,
            &LuaOptions::default(),
        ));
        assert_eq!(
            response,
            serde_json::json!({"n":1,"text":"alpha\nbeta","hits":1})
        );
        assert_eq!(operations.len(), 3);
        let mut host = LuaOptions::default();
        host.max_host_calls = 1;
        assert!(matches!(
            execute(
                "return {x=function(a,c) c.workspace.list_dir({}); c.workspace.list_dir({}); return {response=true} end}",
                &t,
                &call(Value::Null),
                &Map::new(),
                &Workspace::empty(),
                &host
            ),
            LuaExecution::Failed { .. }
        ));
    }

    #[test]
    fn globals_reset_and_bad_shapes_fail() {
        let t = tool("x", SideEffect::Read);
        assert!(matches!(
            execute(
                "return {x=function() leaked=7; return {response=true} end}",
                &t,
                &call(Value::Null),
                &Map::new(),
                &Workspace::empty(),
                &LuaOptions::default()
            ),
            LuaExecution::Computed { .. }
        ));
        assert_eq!(
            computed(execute(
                "return {x=function() return {response=(leaked==nil)} end}",
                &t,
                &call(Value::Null),
                &Map::new(),
                &Workspace::empty(),
                &LuaOptions::default()
            ))
            .0,
            Value::Bool(true)
        );
        for source in [
            "return {",
            "return 2",
            "return {x=3}",
            "return {x=function() return nil end}",
            "return {x=function() return {response=1,state_patch={a=1}} end}",
        ] {
            assert!(matches!(
                execute(
                    source,
                    &t,
                    &call(Value::Null),
                    &Map::new(),
                    &Workspace::empty(),
                    &LuaOptions::default()
                ),
                LuaExecution::Failed { .. }
            ));
        }
    }
}
