//! The simulation workspace: an in-memory filesystem the simulator
//! agent accesses via four tools (read, write, list_dir, grep). It is
//! seeded from an optional uploaded zip and exists even when empty.
//!
//! Per-trace isolation is the central invariant: every scenario run
//! gets its OWN workspace, so writes in one trace never leak into
//! another. To keep that cheap for large seeds (up to 50 MB), the seed
//! is shared by reference (`Arc`) and only a trace's own writes live in
//! a private overlay that shadows the seed. Reads fall through to the
//! shared seed; the 50 MB is paid ONCE, not per scenario.
//!
//! Nothing here touches disk. The zip is decompressed straight into
//! memory with hard caps (compressed and decompressed) and zip-slip
//! rejection, so a malicious or malformed archive cannot escape the
//! workspace root or exhaust memory. This is deterministic
//! bookkeeping — a filesystem — not a parallel intent-compilation
//! system. The simulator LLM decides everything semantic (when to
//! look, what to return); the workspace only stores bytes and answers
//! queries truthfully.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::sync::Arc;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zip::ZipArchive;

use crate::llm::ToolDef;

/// Hard cap on the uploaded (compressed) zip, in bytes. Enforced before
/// and during unpack. Overridable via `PROMPT_EXPLORE_WORKSPACE_COMPRESSED_LIMIT`.
pub const DEFAULT_COMPRESSED_LIMIT: usize = 50 * 1024 * 1024;
/// Hard cap on the total decompressed content, in bytes. Enforced during
/// unpack by reading in chunks and aborting if exceeded (defends against
/// decompression bombs regardless of the sizes declared in the archive).
/// Overridable via `PROMPT_EXPLORE_WORKSPACE_DECOMPRESSED_LIMIT`.
pub const DEFAULT_DECOMPRESSED_LIMIT: usize = 500 * 1024 * 1024;
/// Hard cap on the number of files, to bound pathological archives.
pub const MAX_FILES: usize = 100_000;
/// Default maximum lines returned by one simulator workspace read.
pub const DEFAULT_MAX_READ_LINES: usize = 5000;
/// Default maximum matches returned by one simulator workspace grep.
pub const DEFAULT_MAX_GREP_MATCHES: usize = 1000;
/// Default maximum characters included from one grep result line.
pub const DEFAULT_MAX_LINE_LEN: usize = 2000;
/// Default byte budget used while constructing one workspace tool result.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 1024 * 1024;
/// Process-safety ceiling even for direct library callers or API overrides.
pub const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
/// Reserved private namespace for harness support artifacts. Uploaded application
/// workspaces cannot occupy it; the native simulator uses it for Lua programs.
pub const PRIVATE_NAMESPACE: &str = ".prompt-explore";

/// Bounds on workspace tool output inserted into the simulator conversation.
/// They are per-workspace so an investigation can override them without
/// changing other concurrent runs.
#[derive(Debug, Clone)]
pub struct WorkspaceToolLimits {
    pub max_read_lines: usize,
    pub max_grep_matches: usize,
    pub max_line_len: usize,
    pub max_output_bytes: usize,
}

impl Default for WorkspaceToolLimits {
    fn default() -> Self {
        Self {
            max_read_lines: DEFAULT_MAX_READ_LINES,
            max_grep_matches: DEFAULT_MAX_GREP_MATCHES,
            max_line_len: DEFAULT_MAX_LINE_LEN,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("compressed zip is {size} bytes, which exceeds the {limit}-byte limit")]
    TooLargeCompressed { size: usize, limit: usize },
    #[error("decompressed zip exceeded the {limit}-byte limit (reached {size} bytes)")]
    TooLargeDecompressed { size: usize, limit: usize },
    #[error("zip contains too many entries ({count}); limit is {limit}")]
    TooManyEntries { count: usize, limit: usize },
    #[error("zip entry escapes the workspace root (zip-slip rejected): {0}")]
    PathTraversal(String),
    #[error("zip entry uses reserved private namespace '{PRIVATE_NAMESPACE}': {0}")]
    ReservedPath(String),
    #[error("could not read zip: {0}")]
    BadZip(String),
}

/// The immutable seed: the uploaded files. Shared across every trace by
/// reference, so it is paid for once no matter how many scenarios run.
struct Seed {
    files: BTreeMap<String, Vec<u8>>,
}

/// An in-memory filesystem: a shared immutable seed plus a per-trace
/// overlay of writes (and deletes). `Clone` is cheap — it clones the
/// seed's `Arc` and the (initially empty) overlay — which is exactly how
/// each trace gets its own isolated workspace from one uploaded zip.
#[derive(Clone)]
pub struct Workspace {
    seed: Arc<Seed>,
    /// A trace's own mutations. `Some(bytes)` overwrites the seed at a
    /// path; `None` is a tombstone (a delete) shadowing a seed path.
    overlay: HashMap<String, Option<Vec<u8>>>,
    limits: WorkspaceToolLimits,
}

impl Workspace {
    /// An empty workspace (no seed). This is what runs get when no zip
    /// was uploaded — the four tools still work (writes populate the
    /// overlay), so a purely narrative world can still use the workspace
    /// as scratch memory.
    pub fn empty() -> Self {
        Workspace {
            seed: Arc::new(Seed {
                files: BTreeMap::new(),
            }),
            overlay: HashMap::new(),
            limits: WorkspaceToolLimits::default(),
        }
    }

    /// Apply per-conversation output bounds for workspace tools.
    pub fn with_tool_limits(mut self, limits: WorkspaceToolLimits) -> Self {
        self.limits = limits;
        self
    }

    /// How many files the seed contains (the uploaded count). Used for the
    /// "your simulation workspace currently contains N files" boot line.
    pub fn file_count(&self) -> usize {
        self.seed.files.len()
    }

    /// Whether the seed is empty (no upload, or an empty zip).
    pub fn is_empty(&self) -> bool {
        self.seed.files.is_empty()
    }

    /// The seed files, as (path, bytes) pairs, for surfacing in results
    /// (reproducibility: the caller sees exactly what the simulator saw).
    /// Returns references into the shared seed; cheap.
    pub fn seed_paths(&self) -> Vec<String> {
        self.seed.files.keys().cloned().collect()
    }

    /// Every file currently in the workspace (seed ∪ writes, minus deletes) as
    /// owned (path, bytes) pairs. Used to export a stored scenario's initial
    /// workspace: a content hash alone is not a reproducible workspace.
    pub fn inventory(&self) -> Vec<(String, Vec<u8>)> {
        let mut files = Vec::new();
        self.visit_known_paths(|path| {
            if let Some(bytes) = self.file_bytes(path) {
                files.push((path.to_string(), bytes.to_vec()));
            }
            true
        });
        files
    }

    /// Stable SHA-256 identity of the current workspace contents. Paths are
    /// visited in lexical order and every path/content pair is length-delimited,
    /// so zip ordering, timestamps, compression, and other archive metadata
    /// cannot affect the result. Overlay writes and deletes are included.
    pub fn content_hash(&self) -> String {
        let mut hash = Sha256::new();
        self.visit_known_paths(|path| {
            let bytes = self.file_bytes(path).expect("visited path exists");
            hash.update((path.len() as u64).to_be_bytes());
            hash.update(path.as_bytes());
            hash.update((bytes.len() as u64).to_be_bytes());
            hash.update(bytes);
            true
        });
        format!("{:x}", hash.finalize())
    }

    /// Internal bounded consumers (e.g. the Lua loader) can check size before
    /// copying a file. Unlike the read tool this returns the whole file, not a
    /// line-limited rendering, and never touches the host filesystem.
    pub(crate) fn file_bytes(&self, path: &str) -> Option<&[u8]> {
        if let Some(v) = self.overlay.get(path) {
            return v.as_deref();
        }
        self.seed.files.get(path).map(Vec::as_slice)
    }

    /// Visit currently-existing paths (seed ∪ overlay writes, minus
    /// tombstones) in sorted order without cloning the seed's path strings.
    /// Returning false stops immediately, so byte-bounded list/grep calls do
    /// not first materialize a potentially huge path inventory.
    fn visit_known_paths(&self, mut visitor: impl FnMut(&str) -> bool) {
        let mut seed = self.seed.files.keys().peekable();
        // Overlay keys are normally tiny (writes made during one trace). Sort
        // borrowed references so we can merge them with the seed deterministically.
        let mut overlay_keys: Vec<&String> = self.overlay.keys().collect();
        overlay_keys.sort_unstable();
        let mut overlay = overlay_keys.into_iter().peekable();

        loop {
            let next = match (seed.peek(), overlay.peek()) {
                (Some(seed_path), Some(overlay_path)) => {
                    match seed_path.as_str().cmp(overlay_path.as_str()) {
                        std::cmp::Ordering::Less => Some(seed.next().unwrap().as_str()),
                        std::cmp::Ordering::Greater => {
                            let path = overlay.next().unwrap();
                            self.overlay[path].as_ref().map(|_| path.as_str())
                        }
                        std::cmp::Ordering::Equal => {
                            let path = overlay.next().unwrap();
                            seed.next();
                            self.overlay[path].as_ref().map(|_| path.as_str())
                        }
                    }
                }
                (Some(_), None) => Some(seed.next().unwrap().as_str()),
                (None, Some(_)) => {
                    let path = overlay.next().unwrap();
                    self.overlay[path].as_ref().map(|_| path.as_str())
                }
                (None, None) => break,
            };
            if let Some(path) = next
                && !visitor(path)
            {
                break;
            }
        }
    }

    /// Dispatch one tool call from the simulator against the workspace.
    /// Always returns a JSON value; failures are in-band
    /// (`{"error": "..."}`) so the simulator can see them and react,
    /// exactly as a real tool framework feeds errors back to an agent.
    /// Execute a native simulator workspace tool call. Native simulator calls
    /// may access private harness artifacts; those artifacts are not
    /// application-world inventory.
    pub fn exec(&mut self, tool: &str, args: &Value) -> Value {
        self.exec_bounded(tool, args, self.limits.max_output_bytes)
    }

    /// Execute while imposing a tighter caller-specific result-construction
    /// budget. This internal/native view can access private support files.
    pub(crate) fn exec_bounded(
        &mut self,
        tool: &str,
        args: &Value,
        max_output_bytes: usize,
    ) -> Value {
        self.exec_bounded_with_view(tool, args, max_output_bytes, true)
    }

    /// Execute through the application-facing capability view used by Lua
    /// handlers. It hides and rejects the harness-private namespace.
    pub(crate) fn exec_application_bounded(
        &mut self,
        tool: &str,
        args: &Value,
        max_output_bytes: usize,
    ) -> Value {
        self.exec_bounded_with_view(tool, args, max_output_bytes, false)
    }

    fn exec_bounded_with_view(
        &mut self,
        tool: &str,
        args: &Value,
        max_output_bytes: usize,
        allow_private: bool,
    ) -> Value {
        let max_output_bytes = max_output_bytes
            .min(self.limits.max_output_bytes)
            .min(MAX_OUTPUT_BYTES);
        match tool {
            "read" => self.exec_read_bounded(args, max_output_bytes, allow_private),
            "list_dir" => self.exec_list_dir_bounded(args, max_output_bytes, allow_private),
            "grep" => self.exec_grep_bounded(args, max_output_bytes, allow_private),
            "write" => self.exec_write_bounded(args, allow_private),
            other => json!({ "error": format!("unknown workspace tool '{other}'") }),
        }
    }

    #[cfg(test)]
    fn exec_read(&self, args: &Value) -> Value {
        self.exec_read_bounded(
            args,
            self.limits.max_output_bytes.min(MAX_OUTPUT_BYTES),
            true,
        )
    }

    fn exec_read_bounded(
        &self,
        args: &Value,
        max_output_bytes: usize,
        allow_private: bool,
    ) -> Value {
        let raw_path = match str_arg(args, "path") {
            Some(p) => p,
            None => return json!({ "error": "missing required argument 'path'" }),
        };
        let path = match normalize(raw_path) {
            Some(p) => p,
            None => return json!({ "path": raw_path, "error": "invalid path" }),
        };
        if !allow_private && is_private_path(&path) {
            return private_path_error(raw_path);
        }
        match self.file_bytes(&path) {
            None => json!({ "path": raw_path, "error": "not found" }),
            Some(bytes) => {
                // Count and slice borrowed byte lines; never clone or decode the
                // whole file before applying the byte budget. Invalid UTF-8 is
                // rendered lossily only after the selected bytes are bounded.
                let total = bytes.split(|byte| *byte == b'\n').count();
                let start = usize_arg(args, "start_line").unwrap_or(1).max(1);
                let cap_end = start
                    .saturating_add(self.limits.max_read_lines)
                    .saturating_sub(1);
                let requested_end = usize_arg(args, "end_line").unwrap_or(cap_end);
                let end = requested_end.min(cap_end);
                if end < start {
                    return json!({
                        "path": raw_path,
                        "error": "end_line is before start_line",
                    });
                }
                if start > total {
                    return json!({
                        "path": raw_path,
                        "content": "",
                        "start_line": start,
                        "end_line": start - 1,
                        "total_lines": total,
                        "truncated": false,
                        "note": "start_line is beyond the end of the file"
                    });
                }

                let last = end.min(total);
                let mut selected = Vec::with_capacity(max_output_bytes.min(8192));
                let mut actual_end = start - 1;
                let mut byte_truncated = false;
                for (index, raw_line) in bytes
                    .split(|byte| *byte == b'\n')
                    .enumerate()
                    .skip(start - 1)
                    .take(last - start + 1)
                {
                    let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
                    if actual_end >= start {
                        if selected.len() == max_output_bytes {
                            byte_truncated = true;
                            break;
                        }
                        selected.push(b'\n');
                    }
                    let remaining = max_output_bytes.saturating_sub(selected.len());
                    let take = line.len().min(remaining);
                    selected.extend_from_slice(&line[..take]);
                    actual_end = index + 1;
                    if take < line.len() {
                        byte_truncated = true;
                        break;
                    }
                }
                let content = bounded_lossy(&selected, max_output_bytes);
                json!({
                    "path": raw_path,
                    "content": content,
                    "start_line": start,
                    "end_line": actual_end,
                    "total_lines": total,
                    "truncated": byte_truncated || actual_end < total,
                })
            }
        }
    }

    #[cfg(test)]
    fn exec_list_dir(&self, args: &Value) -> Value {
        self.exec_list_dir_bounded(
            args,
            self.limits.max_output_bytes.min(MAX_OUTPUT_BYTES),
            true,
        )
    }

    fn exec_list_dir_bounded(
        &self,
        args: &Value,
        max_output_bytes: usize,
        allow_private: bool,
    ) -> Value {
        let raw = str_arg(args, "path").unwrap_or("");
        // Both conventional root aliases are accepted only for directory
        // listing. Other paths must be safe workspace-relative paths.
        let dir = if raw.trim().is_empty() || raw.trim() == "." {
            String::new()
        } else {
            match normalize(raw) {
                Some(p) => p,
                None => return json!({ "path": raw, "error": "invalid path" }),
            }
        };
        if !allow_private && is_private_path(&dir) {
            return private_path_error(raw);
        }
        // If the path is itself a file, it is not a directory.
        if !dir.is_empty() && self.file_bytes(&dir).is_some() {
            return json!({ "path": raw, "error": "not a directory" });
        }
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        // name -> kind; a name shown as a directory by any path wins.
        let mut entries: BTreeMap<String, &'static str> = BTreeMap::new();
        let mut output_bytes = raw.len().saturating_add(64);
        let mut truncated = false;
        self.visit_known_paths(|p| {
            if !allow_private && is_private_path(p) {
                return true;
            }
            let rel = if prefix.is_empty() {
                p
            } else {
                match p.strip_prefix(&prefix) {
                    Some(r) => r,
                    None => return true,
                }
            };
            let (first, rest) = match rel.find('/') {
                Some(i) => (&rel[..i], &rel[i..]),
                None => (rel, ""),
            };
            if first.is_empty() {
                return true;
            }
            let kind = if rest.is_empty() { "file" } else { "dir" };
            if let Some(slot) = entries.get_mut(first) {
                if kind == "dir" {
                    *slot = "dir";
                }
                return true;
            }
            let entry_bytes = first.len().saturating_add(32);
            if entry_bytes > max_output_bytes.saturating_sub(output_bytes) {
                truncated = true;
                return false;
            }
            output_bytes += entry_bytes;
            entries.insert(first.to_string(), kind);
            true
        });
        if !dir.is_empty() && entries.is_empty() && !truncated {
            return json!({ "path": raw, "error": "not found" });
        }
        let arr: Vec<Value> = entries
            .iter()
            .map(|(n, k)| json!({ "name": n, "kind": k }))
            .collect();
        json!({ "path": raw, "entries": arr, "truncated": truncated })
    }

    #[cfg(test)]
    fn exec_grep(&self, args: &Value) -> Value {
        self.exec_grep_bounded(
            args,
            self.limits.max_output_bytes.min(MAX_OUTPUT_BYTES),
            true,
        )
    }

    fn exec_grep_bounded(
        &self,
        args: &Value,
        max_output_bytes: usize,
        allow_private: bool,
    ) -> Value {
        let pattern = match str_arg(args, "pattern") {
            Some(p) => p.to_string(),
            None => return json!({ "error": "missing required argument 'pattern'" }),
        };
        let case_insensitive = bool_arg(args, "case_insensitive").unwrap_or(false);
        let root = match str_arg(args, "path").map(str::trim) {
            None | Some("") | Some(".") => None,
            Some(raw) => match normalize(raw) {
                Some(path) => Some(path),
                None => return json!({ "path": raw, "error": "invalid path" }),
            },
        };
        if !allow_private && root.as_deref().is_some_and(is_private_path) {
            return private_path_error(str_arg(args, "path").unwrap_or_default());
        }
        // `path` may name a file (match exactly that one path) or a
        // directory (match everything under it). Unset = whole workspace.
        let in_scope = |p: &str| match &root {
            None => true,
            Some(r) => p == r.as_str() || p.starts_with(&format!("{r}/")),
        };
        let needle = if case_insensitive {
            pattern.to_lowercase()
        } else {
            pattern.clone()
        };
        let mut matches: Vec<Value> = Vec::new();
        let mut output_bytes = pattern.len().saturating_add(64);
        let mut truncated = false;
        self.visit_known_paths(|p| {
            if (!allow_private && is_private_path(p)) || !in_scope(p) {
                return true;
            }
            let Some(bytes) = self.file_bytes(p) else {
                return true;
            };
            for (i, raw_line) in bytes.split(|byte| *byte == b'\n').enumerate() {
                let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
                let hit = if case_insensitive {
                    contains_unicode_case_insensitive(line, &needle)
                } else {
                    contains_bytes(line, needle.as_bytes())
                };
                if hit {
                    let fixed_bytes = p.len().saturating_add(48);
                    let remaining = max_output_bytes.saturating_sub(output_bytes);
                    if fixed_bytes >= remaining {
                        truncated = true;
                        return false;
                    }
                    let text = truncate_line_bytes(
                        line,
                        self.limits.max_line_len,
                        remaining - fixed_bytes,
                    );
                    let match_bytes = fixed_bytes.saturating_add(text.len());
                    output_bytes += match_bytes;
                    matches.push(json!({
                        "path": p,
                        "line": i + 1,
                        "text": text,
                    }));
                    if matches.len() >= self.limits.max_grep_matches {
                        truncated = true;
                        return false;
                    }
                }
            }
            true
        });
        json!({
            "pattern": pattern,
            "matches": matches,
            "truncated": truncated,
        })
    }

    #[cfg(test)]
    fn exec_write(&mut self, args: &Value) -> Value {
        self.exec_write_bounded(args, true)
    }

    fn exec_write_bounded(&mut self, args: &Value, allow_private: bool) -> Value {
        let raw_path = match str_arg(args, "path") {
            Some(p) => p,
            None => return json!({ "error": "missing required argument 'path'" }),
        };
        let content = match str_arg(args, "content") {
            Some(c) => c,
            None => return json!({ "error": "missing required argument 'content'" }),
        };
        let path = match normalize(raw_path) {
            Some(p) => p,
            None => return json!({ "path": raw_path, "error": "invalid path" }),
        };
        if !allow_private && is_private_path(&path) {
            return private_path_error(raw_path);
        }
        let bytes = content.as_bytes().to_vec();
        let n = bytes.len();
        self.overlay.insert(path, Some(bytes));
        json!({ "path": raw_path, "bytes": n, "ok": true })
    }

    /// The four tools the simulator may call, as `ToolDef`s suitable for
    /// the chat request's `tools` field. Descriptions are written for the
    /// simulator: they explain how to use each tool, not the policy for
    /// when (that lives in the world narrative).
    pub fn tool_defs(&self) -> Vec<ToolDef> {
        vec![
            ToolDef {
                name: "list_dir".into(),
                description: "List the direct children of a directory in your simulation \
                              workspace. Returns {\"path\":..., \"entries\":[{\"name\":..., \
                              \"kind\":\"file\"|\"dir\"}], \"truncated\":bool}, or {\"error\":\"not found\"}. \
                              Omit \"path\" (or pass \"\" or \".\") for the workspace root. Use this \
                              to discover structure before reading."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Directory path relative to the workspace root. Omit, \"\", or \".\" for the root."
                        }
                    }
                }),
            },
            ToolDef {
                name: "read".into(),
                description: format!(
                    "Read up to {} lines of a file from your simulation workspace. \
                     Returns {{\"path\":..., \"content\":..., \"start_line\":..., \
                     \"end_line\":..., \"total_lines\":..., \"truncated\":bool}}, or \
                     {{\"path\":..., \"error\":\"not found\"}}. Paths are relative to \
                     the workspace root and use '/' separators. Construction is also byte-bounded. Use list_dir first if \
                     you do not know the exact path.",
                    self.limits.max_read_lines,
                ),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File path, relative to the workspace root." },
                        "start_line": { "type": "integer", "description": "1-based first line to return (optional)." },
                        "end_line": { "type": "integer", "description": "1-based last line to return (optional)." }
                    },
                    "required": ["path"]
                }),
            },
            ToolDef {
                name: "grep".into(),
                description: format!(
                    "Search your simulation workspace for a literal substring. Returns \
                     {{\"pattern\":..., \"matches\":[{{\"path\":..., \"line\":..., \
                     \"text\":...}}], \"truncated\":bool}} (at most {} matches; results are also byte-bounded). The \
                     \"pattern\" is a LITERAL substring, not a regex. Use this to find \
                     where something is defined or referenced.",
                    self.limits.max_grep_matches,
                ),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "Literal substring to search for." },
                        "path": { "type": "string", "description": "Optional directory/file prefix to restrict the search to." },
                        "case_insensitive": { "type": "boolean", "description": "Match case-insensitively (default false)." }
                    },
                    "required": ["pattern"]
                }),
            },
            ToolDef {
                name: "write".into(),
                description: "Write a file in your simulation workspace (create or overwrite). \
                              The workspace is EPHEMERAL and PRIVATE to this run: the agent you \
                              are simulating never sees it — only your tool responses reach it. \
                              Use it as scratch memory, e.g. to record generated content so later \
                              reads of the same path stay consistent."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File path to write, relative to the workspace root (creates or overwrites)." },
                        "content": { "type": "string", "description": "The full new contents of the file." }
                    },
                    "required": ["path", "content"]
                }),
            },
        ]
    }
}

/// Decompress a zip entirely in memory and return a workspace seeded with
/// its files. Nothing is written to disk. Hard caps (compressed and
/// decompressed) and zip-slip rejection make a malicious or malformed
/// archive safe: it cannot escape the workspace root or exhaust memory.
pub fn unpack_zip(bytes: &[u8]) -> Result<Workspace, WorkspaceError> {
    unpack_zip_with_limits(bytes, DEFAULT_COMPRESSED_LIMIT, DEFAULT_DECOMPRESSED_LIMIT)
}

/// Same as `unpack_zip` but with caller-chosen compressed and decompressed
/// limits (in bytes). The defaults are `DEFAULT_COMPRESSED_LIMIT` and
/// `DEFAULT_DECOMPRESSED_LIMIT` (50 MB / 500 MB).
pub fn unpack_zip_with_limits(
    bytes: &[u8],
    compressed_limit: usize,
    decompressed_limit: usize,
) -> Result<Workspace, WorkspaceError> {
    if bytes.len() > compressed_limit {
        return Err(WorkspaceError::TooLargeCompressed {
            size: bytes.len(),
            limit: compressed_limit,
        });
    }
    let cursor = std::io::Cursor::new(bytes);
    let mut archive = ZipArchive::new(cursor).map_err(|e| WorkspaceError::BadZip(e.to_string()))?;
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut total_decompressed: usize = 0;
    for i in 0..archive.len() {
        if files.len() >= MAX_FILES {
            return Err(WorkspaceError::TooManyEntries {
                count: files.len(),
                limit: MAX_FILES,
            });
        }
        let mut entry = archive
            .by_index(i)
            .map_err(|e| WorkspaceError::BadZip(e.to_string()))?;
        // Directory entries carry no content; the tree is inferred from
        // file paths, so skip them.
        if entry.is_dir() {
            continue;
        }
        let raw_name = entry.name().to_string();
        let path =
            normalize(&raw_name).ok_or_else(|| WorkspaceError::PathTraversal(raw_name.clone()))?;
        if is_private_path(&path) {
            return Err(WorkspaceError::ReservedPath(raw_name));
        }
        // Read in bounded chunks: the running total defends against
        // decompression bombs regardless of the sizes the archive
        // declares. If total ever exceeds the cap, abort.
        let mut buf = Vec::new();
        let mut chunk = [0u8; 65536];
        loop {
            let n = entry
                .read(&mut chunk)
                .map_err(|e| WorkspaceError::BadZip(e.to_string()))?;
            if n == 0 {
                break;
            }
            total_decompressed += n;
            if total_decompressed > decompressed_limit {
                return Err(WorkspaceError::TooLargeDecompressed {
                    size: total_decompressed,
                    limit: decompressed_limit,
                });
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        // Last entry at a path wins on duplicate (benign for normal zips).
        files.insert(path, buf);
    }
    Ok(Workspace {
        seed: Arc::new(Seed { files }),
        overlay: HashMap::new(),
        limits: WorkspaceToolLimits::default(),
    })
}

/// Normalize a safe workspace-relative path. Empty paths and root aliases are
/// handled by the caller where their semantics are defined. Absolute paths,
/// traversal, NULs, and Windows drive-qualified paths are rejected.
fn normalize(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty()
        || raw.starts_with(['/', '\\'])
        || (raw.len() >= 2 && raw.as_bytes()[0].is_ascii_alphabetic() && raw.as_bytes()[1] == b':')
    {
        return None;
    }
    // Accept backslash separators (Windows-style) after rejecting absolute
    // backslash paths above.
    let replaced = raw.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for comp in replaced.split('/') {
        match comp {
            "" | "." => continue,
            ".." => return None,
            _ => {
                if comp.contains('\0') {
                    return None;
                }
                parts.push(comp);
            }
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

fn is_private_path(path: &str) -> bool {
    path == PRIVATE_NAMESPACE
        || path
            .strip_prefix(PRIVATE_NAMESPACE)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn private_path_error(raw_path: &str) -> Value {
    json!({
        "path": raw_path,
        "error": "private harness path is not available to application handlers",
    })
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

fn usize_arg(args: &Value, key: &str) -> Option<usize> {
    args.get(key).and_then(|v| v.as_u64()).map(|n| n as usize)
}

fn bool_arg(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(|v| v.as_bool())
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty()
        || haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn contains_unicode_case_insensitive(haystack: &[u8], folded_needle: &str) -> bool {
    let Ok(haystack) = std::str::from_utf8(haystack) else {
        // Invalid UTF-8 has no well-defined Unicode case mapping. Preserve
        // bounded, allocation-free behavior for its ASCII portions.
        return contains_ascii_case_insensitive(haystack, folded_needle.as_bytes());
    };
    let needle: Vec<char> = folded_needle.chars().collect();
    if needle.is_empty() {
        return true;
    }

    // KMP over streaming Unicode lowercase expansion preserves the previous
    // Unicode-aware semantics without allocating a lowercased copy of a
    // potentially enormous source line.
    let mut prefix = vec![0usize; needle.len()];
    let mut matched = 0usize;
    for i in 1..needle.len() {
        while matched > 0 && needle[i] != needle[matched] {
            matched = prefix[matched - 1];
        }
        if needle[i] == needle[matched] {
            matched += 1;
        }
        prefix[i] = matched;
    }
    matched = 0;
    for ch in haystack.chars().flat_map(char::to_lowercase) {
        while matched > 0 && ch != needle[matched] {
            matched = prefix[matched - 1];
        }
        if ch == needle[matched] {
            matched += 1;
            if matched == needle.len() {
                return true;
            }
        }
    }
    false
}

fn contains_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty()
        || haystack
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle))
}

fn bounded_lossy(bytes: &[u8], max_bytes: usize) -> String {
    let lossy = String::from_utf8_lossy(bytes);
    truncate_utf8_bytes(&lossy, max_bytes).to_string()
}

fn truncate_line_bytes(line: &[u8], max_chars: usize, max_bytes: usize) -> String {
    if max_chars == 0 || max_bytes == 0 {
        return String::new();
    }
    // Bound potentially lossy decoding before it can allocate. Replacement
    // characters can expand invalid bytes, so enforce the output byte cap too.
    let input = &line[..line.len().min(max_bytes)];
    let lossy = String::from_utf8_lossy(input);
    let mut chars = lossy.chars();
    let mut text: String = chars.by_ref().take(max_chars).collect();
    let truncated = input.len() < line.len() || chars.next().is_some();
    if truncated {
        text.push_str("… <truncated>");
    }
    truncate_utf8_bytes(&text, max_bytes).to_string()
}

fn truncate_utf8_bytes(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn ws(files: &[(&str, &str)]) -> Workspace {
        let mut map = BTreeMap::new();
        for (k, v) in files {
            map.insert((*k).to_string(), v.as_bytes().to_vec());
        }
        Workspace {
            seed: Arc::new(Seed { files: map }),
            overlay: HashMap::new(),
            limits: WorkspaceToolLimits::default(),
        }
    }

    #[test]
    fn normalize_rejects_traversal() {
        assert_eq!(normalize("src/main.rs").as_deref(), Some("src/main.rs"));
        assert_eq!(normalize("./src/./a.rs").as_deref(), Some("src/a.rs"));
        assert_eq!(normalize("src\\main.rs").as_deref(), Some("src/main.rs"));
        assert_eq!(normalize("../etc/passwd"), None);
        assert_eq!(normalize("a/../../b"), None);
        assert_eq!(normalize("/src/main.rs"), None);
        assert_eq!(normalize("\\\\server\\share"), None);
        assert_eq!(normalize("C:\\temp\\file"), None);
        assert_eq!(normalize(""), None);
        assert_eq!(normalize("/"), None);
        assert_eq!(normalize("a\0b"), None);
    }

    #[test]
    fn read_returns_lines_and_not_found() {
        let w = ws(&[("a.txt", "line1\nline2\nline3")]);
        let r = w.exec_read(&json!({"path": "a.txt"}));
        assert_eq!(r["content"], json!("line1\nline2\nline3"));
        assert_eq!(r["total_lines"], json!(3));
        assert_eq!(r["truncated"], json!(false));
        let r = w.exec_read(&json!({"path": "a.txt", "start_line": 2, "end_line": 2}));
        assert_eq!(r["content"], json!("line2"));
        let r = w.exec_read(&json!({"path": "missing"}));
        assert_eq!(r["error"], json!("not found"));
    }

    #[test]
    fn read_caps_at_max_lines() {
        let big = (0..DEFAULT_MAX_READ_LINES + 50)
            .map(|i| format!("l{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let w = ws(&[("big.txt", &big)]);
        let r = w.exec_read(&json!({"path": "big.txt"}));
        assert_eq!(r["end_line"], json!(DEFAULT_MAX_READ_LINES));
        assert_eq!(r["truncated"], json!(true));
    }

    #[test]
    fn list_dir_groups_children() {
        let w = ws(&[
            ("src/main.rs", ""),
            ("src/util.rs", ""),
            ("README.md", ""),
            ("src/nested/deep.rs", ""),
        ]);
        let root = w.exec_list_dir(&json!({}));
        let root_dot = w.exec_list_dir(&json!({"path":"."}));
        let root_empty = w.exec_list_dir(&json!({"path":""}));
        assert_eq!(root_dot["entries"], root["entries"]);
        assert_eq!(root_empty["entries"], root["entries"]);
        assert_eq!(root_dot["truncated"], root["truncated"]);
        assert_eq!(root_empty["truncated"], root["truncated"]);
        let names: Vec<&str> = root["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["README.md", "src"]);
        let kinds: std::collections::HashMap<&str, &str> = root["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| (e["name"].as_str().unwrap(), e["kind"].as_str().unwrap()))
            .collect();
        assert_eq!(kinds["README.md"], "file");
        assert_eq!(kinds["src"], "dir");

        let src = w.exec_list_dir(&json!({"path": "src"}));
        let names: Vec<&str> = src["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["main.rs", "nested", "util.rs"]);

        // Listing a file path errors.
        let err = w.exec_list_dir(&json!({"path": "README.md"}));
        assert_eq!(err["error"], json!("not a directory"));
    }

    #[test]
    fn tool_output_limits_are_overridable_per_workspace() {
        let w = ws(&[("a.txt", "one\ntwo\nthree")]).with_tool_limits(WorkspaceToolLimits {
            max_read_lines: 1,
            max_grep_matches: 1,
            max_line_len: 2,
            max_output_bytes: 1024,
        });
        let read = w.exec_read(&json!({"path": "a.txt"}));
        assert_eq!(read["content"], json!("one"));
        assert_eq!(read["truncated"], json!(true));

        let grep = w.exec_grep(&json!({"pattern": "t"}));
        assert_eq!(grep["matches"].as_array().unwrap().len(), 1);
        assert_eq!(grep["matches"][0]["text"], json!("tw… <truncated>"));
        assert_eq!(grep["truncated"], json!(true));

        let defs = w.tool_defs();
        assert!(
            defs.iter()
                .find(|d| d.name == "read")
                .unwrap()
                .description
                .contains("up to 1 lines")
        );
        assert!(
            defs.iter()
                .find(|d| d.name == "grep")
                .unwrap()
                .description
                .contains("at most 1 matches")
        );
    }

    #[test]
    fn byte_budget_bounds_huge_single_lines_before_rendering() {
        let huge = format!("{}NEEDLE", "x".repeat(8 * 1024 * 1024));
        let mut w = ws(&[("huge.txt", &huge)]).with_tool_limits(WorkspaceToolLimits {
            max_output_bytes: 1024,
            ..WorkspaceToolLimits::default()
        });

        let read = w.exec("read", &json!({"path": "huge.txt"}));
        assert!(read["content"].as_str().unwrap().len() <= 1024);
        assert_eq!(read["truncated"], true);

        // A tighter caller (the Lua bridge) wins over the workspace setting.
        let tighter = w.exec_bounded("read", &json!({"path": "huge.txt"}), 64);
        assert!(tighter["content"].as_str().unwrap().len() <= 64);
        assert_eq!(tighter["truncated"], true);

        // Grep searches borrowed bytes and only materializes a bounded preview,
        // even when the match is at the end of one enormous line.
        let grep = w.exec("grep", &json!({"pattern": "NEEDLE"}));
        assert_eq!(grep["matches"].as_array().unwrap().len(), 1);
        assert!(grep["matches"][0]["text"].as_str().unwrap().len() <= 1024);

        let many: Vec<(String, String)> = (0..1000)
            .map(|i| (format!("dir/{i:04}-{}", "n".repeat(80)), String::new()))
            .collect();
        let borrowed: Vec<(&str, &str)> = many
            .iter()
            .map(|(path, content)| (path.as_str(), content.as_str()))
            .collect();
        let listed = ws(&borrowed)
            .with_tool_limits(WorkspaceToolLimits {
                max_output_bytes: 1024,
                ..WorkspaceToolLimits::default()
            })
            .exec_list_dir(&json!({"path": "dir"}));
        assert_eq!(listed["truncated"], true);
        assert!(listed["entries"].as_array().unwrap().len() < 1000);
    }

    #[test]
    fn grep_finds_substrings() {
        let w = ws(&[
            ("src/a.rs", "fn alpha() {}\nfn beta() {}\n"),
            ("src/b.rs", "alpha used here\n"),
            ("src/unicode.rs", "Ärger\n"),
        ]);
        let r = w.exec_grep(&json!({"pattern": "alpha"}));
        let ms = r["matches"].as_array().unwrap();
        assert_eq!(ms.len(), 2);
        assert_eq!(ms[0]["path"], json!("src/a.rs"));
        assert_eq!(ms[0]["line"], json!(1));
        assert_eq!(ms[1]["path"], json!("src/b.rs"));

        // Scoped to a prefix.
        let r = w.exec_grep(&json!({"pattern": "alpha", "path": "src/b.rs"}));
        assert_eq!(r["matches"].as_array().unwrap().len(), 1);

        // Case-insensitive.
        let r = w.exec_grep(&json!({"pattern": "ALPHA", "case_insensitive": true}));
        assert_eq!(r["matches"].as_array().unwrap().len(), 2);
        let r = w.exec_grep(&json!({"pattern": "ärGER", "case_insensitive": true}));
        assert_eq!(r["matches"].as_array().unwrap().len(), 1);
        assert_eq!(r["matches"][0]["path"], "src/unicode.rs");
    }

    #[test]
    fn write_then_read_roundtrips_and_isolates_per_clone() {
        let base = ws(&[("seed.txt", "original")]);
        // Each trace clones the seed before writing, so its writes never
        // leak back into the shared seed or into a sibling trace.
        let mut w = base.clone();
        // Write overlays the seed.
        let res = w.exec_write(&json!({"path": "made_up.txt", "content": "hello"}));
        assert_eq!(res["ok"], json!(true));
        assert_eq!(
            w.exec_read(&json!({"path": "made_up.txt"}))["content"],
            json!("hello")
        );
        // Overwriting a seed path returns the new content.
        w.exec_write(&json!({"path": "seed.txt", "content": "changed"}));
        assert_eq!(
            w.exec_read(&json!({"path": "seed.txt"}))["content"],
            json!("changed")
        );

        // A fresh clone (another trace) sees neither write.
        let other = base.clone();
        assert_eq!(
            other.exec_read(&json!({"path": "made_up.txt"}))["error"],
            json!("not found")
        );
        assert_eq!(
            other.exec_read(&json!({"path": "seed.txt"}))["content"],
            json!("original")
        );
    }

    #[test]
    fn content_hash_is_sorted_and_tracks_overlay_content() {
        let a = ws(&[("b.txt", "two"), ("a.txt", "one")]);
        let b = ws(&[("a.txt", "one"), ("b.txt", "two")]);
        assert_eq!(a.content_hash(), b.content_hash());
        let original = a.content_hash();
        let mut changed = a.clone();
        changed.exec_write(&json!({"path": "a.txt", "content": "changed"}));
        assert_ne!(original, changed.content_hash());
    }

    #[test]
    fn unpack_zip_in_memory_roundtrip() {
        // Build a zip entirely in memory.
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default();
            zw.start_file("hello.txt", opts).unwrap();
            zw.write_all(b"hi there").unwrap();
            zw.start_file("src/main.rs", opts).unwrap();
            zw.write_all(b"fn main() {}").unwrap();
            zw.finish().unwrap();
        }
        let w = unpack_zip(&buf).expect("unpack");
        assert_eq!(w.file_count(), 2);
        assert_eq!(
            w.exec_read(&json!({"path": "hello.txt"}))["content"],
            json!("hi there")
        );
        assert_eq!(
            w.exec_read(&json!({"path": "src/main.rs"}))["content"],
            json!("fn main() {}")
        );
        assert!(w.seed_paths().contains(&"src/main.rs".to_string()));
    }

    #[test]
    fn unpack_rejects_compressed_over_limit() {
        let bytes = vec![0u8; DEFAULT_COMPRESSED_LIMIT + 1];
        assert!(matches!(
            unpack_zip(&bytes),
            Err(WorkspaceError::TooLargeCompressed { .. })
        ));
    }

    #[test]
    fn unpack_rejects_private_namespace_entry() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default();
            zw.start_file(".prompt-explore/tools.lua", opts).unwrap();
            zw.write_all(b"caller source").unwrap();
            zw.finish().unwrap();
        }
        assert!(matches!(
            unpack_zip(&buf),
            Err(WorkspaceError::ReservedPath(path)) if path == ".prompt-explore/tools.lua"
        ));
    }

    #[test]
    fn unpack_rejects_traversal_entry() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default();
            // Manually craft an entry with a traversal name.
            zw.start_file("../escape.txt", opts).unwrap();
            zw.write_all(b"x").unwrap();
            zw.finish().unwrap();
        }
        assert!(matches!(
            unpack_zip(&buf),
            Err(WorkspaceError::PathTraversal(_))
        ));
    }
}
