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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read;
use std::sync::Arc;

use serde_json::{Value, json};
use zip::ZipArchive;

use crate::llm::ToolDef;

/// Hard cap on the uploaded (compressed) zip, in bytes. Enforced before
/// and during unpack.
pub const COMPRESSED_LIMIT: usize = 5 * 1024 * 1024;
/// Hard cap on the total decompressed content, in bytes. Enforced during
/// unpack by reading in chunks and aborting if exceeded (defends against
/// decompression bombs regardless of the sizes declared in the archive).
pub const DECOMPRESSED_LIMIT: usize = 50 * 1024 * 1024;
/// Hard cap on the number of files, to bound pathological archives.
pub const MAX_FILES: usize = 100_000;

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
        }
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

    /// Resolve a path to its current bytes: the trace's overlay (write or
    /// tombstone) takes precedence, then the shared seed. Returns a clone
    /// so callers need not fight lifetimes across the two stores.
    fn content(&self, path: &str) -> Option<Vec<u8>> {
        if let Some(v) = self.overlay.get(path) {
            return v.clone();
        }
        self.seed.files.get(path).cloned()
    }

    /// All currently-existing paths (seed ∪ overlay writes, minus
    /// tombstones), sorted. Drives list_dir and grep.
    fn known_paths(&self) -> Vec<String> {
        let mut set: BTreeSet<String> = self.seed.files.keys().cloned().collect();
        for (k, v) in &self.overlay {
            if v.is_some() {
                set.insert(k.clone());
            } else {
                set.remove(k);
            }
        }
        set.into_iter().collect()
    }

    /// Dispatch one tool call from the simulator against the workspace.
    /// Always returns a JSON value; failures are in-band
    /// (`{"error": "..."}`) so the simulator can see them and react,
    /// exactly as a real tool framework feeds errors back to an agent.
    pub fn exec(&mut self, tool: &str, args: &Value) -> Value {
        match tool {
            "read" => self.exec_read(args),
            "list_dir" => self.exec_list_dir(args),
            "grep" => self.exec_grep(args),
            "write" => self.exec_write(args),
            other => json!({ "error": format!("unknown workspace tool '{other}'") }),
        }
    }

    fn exec_read(&self, args: &Value) -> Value {
        let raw_path = match str_arg(args, "path") {
            Some(p) => p,
            None => return json!({ "error": "missing required argument 'path'" }),
        };
        let path = match normalize(raw_path) {
            Some(p) => p,
            None => return json!({ "path": raw_path, "error": "invalid path" }),
        };
        match self.content(&path) {
            None => json!({ "path": raw_path, "error": "not found" }),
            Some(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                let lines: Vec<&str> = text.split('\n').collect();
                let total = lines.len();
                let start = usize_arg(args, "start_line").unwrap_or(1).max(1);
                // Cap how many lines one read can return, so a single
                // huge file cannot drown the simulator's context.
                let cap_end = start.saturating_add(MAX_READ_LINES).saturating_sub(1);
                let requested_end = usize_arg(args, "end_line").unwrap_or(cap_end);
                let end = requested_end.min(cap_end);
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
                let slice: Vec<String> = lines[(start - 1)..last]
                    .iter()
                    .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
                    .collect();
                let actual_end = start + slice.len() - 1;
                let content = slice.join("\n");
                json!({
                    "path": raw_path,
                    "content": content,
                    "start_line": start,
                    "end_line": actual_end,
                    "total_lines": total,
                    "truncated": actual_end < total,
                })
            }
        }
    }

    fn exec_list_dir(&self, args: &Value) -> Value {
        let raw = str_arg(args, "path").unwrap_or("");
        // The root is the empty string; normalize any other path.
        let dir = if raw.trim().is_empty() {
            String::new()
        } else {
            match normalize(raw) {
                Some(p) => p,
                None => return json!({ "path": raw, "error": "invalid path" }),
            }
        };
        // If the path is itself a file, it is not a directory.
        if !dir.is_empty() && self.content(&dir).is_some() {
            return json!({ "path": raw, "error": "not a directory" });
        }
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        // name -> kind; a name shown as a directory by any path wins.
        let mut entries: BTreeMap<String, &'static str> = BTreeMap::new();
        for p in self.known_paths() {
            let rel = if prefix.is_empty() {
                p.as_str()
            } else {
                match p.strip_prefix(&prefix) {
                    Some(r) => r,
                    None => continue,
                }
            };
            let (first, rest) = match rel.find('/') {
                Some(i) => (&rel[..i], &rel[i..]),
                None => (rel, ""),
            };
            if first.is_empty() {
                continue;
            }
            let kind = if rest.is_empty() { "file" } else { "dir" };
            let slot = entries.entry(first.to_string()).or_insert("file");
            if kind == "dir" {
                *slot = "dir";
            }
        }
        if !dir.is_empty() && entries.is_empty() {
            return json!({ "path": raw, "error": "not found" });
        }
        let arr: Vec<Value> = entries
            .iter()
            .map(|(n, k)| json!({ "name": n, "kind": k }))
            .collect();
        json!({ "path": raw, "entries": arr })
    }

    fn exec_grep(&self, args: &Value) -> Value {
        let pattern = match str_arg(args, "pattern") {
            Some(p) => p.to_string(),
            None => return json!({ "error": "missing required argument 'pattern'" }),
        };
        let case_insensitive = bool_arg(args, "case_insensitive").unwrap_or(false);
        let root = str_arg(args, "path")
            .filter(|p| !p.trim().is_empty())
            .and_then(normalize);
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
        let mut truncated = false;
        'outer: for p in self.known_paths() {
            if !in_scope(&p) {
                continue;
            }
            let Some(bytes) = self.content(&p) else { continue };
            let text = String::from_utf8_lossy(&bytes);
            for (i, line) in text.split('\n').enumerate() {
                let line = line.strip_suffix('\r').unwrap_or(line);
                let hit = if case_insensitive {
                    line.to_lowercase().contains(&needle)
                } else {
                    line.contains(needle.as_str())
                };
                if hit {
                    matches.push(json!({
                        "path": p,
                        "line": i + 1,
                        "text": truncate_line(line),
                    }));
                    if matches.len() >= MAX_GREP_MATCHES {
                        truncated = true;
                        break 'outer;
                    }
                }
            }
        }
        json!({
            "pattern": pattern,
            "matches": matches,
            "truncated": truncated,
        })
    }

    fn exec_write(&mut self, args: &Value) -> Value {
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
        let bytes = content.as_bytes().to_vec();
        let n = bytes.len();
        self.overlay.insert(path, Some(bytes));
        json!({ "path": raw_path, "bytes": n, "ok": true })
    }

    /// The four tools the simulator may call, as `ToolDef`s suitable for
    /// the chat request's `tools` field. Descriptions are written for the
    /// simulator: they explain how to use each tool, not the policy for
    /// when (that lives in the world narrative).
    pub fn tool_defs() -> Vec<ToolDef> {
        vec![
            ToolDef {
                name: "list_dir".into(),
                description: "List the direct children of a directory in your simulation \
                              workspace. Returns {\"path\":..., \"entries\":[{\"name\":..., \
                              \"kind\":\"file\"|\"dir\"}]}, or {\"error\":\"not found\"}. \
                              Omit \"path\" (or pass \"\") for the workspace root. Use this \
                              to discover structure before reading."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Directory path relative to the workspace root. Omit for the root."
                        }
                    }
                }),
            },
            ToolDef {
                name: "read".into(),
                description: "Read up to 2000 lines of a file from your simulation workspace. \
                              Returns {\"path\":..., \"content\":..., \"start_line\":..., \
                              \"end_line\":..., \"total_lines\":..., \"truncated\":bool}, or \
                              {\"path\":..., \"error\":\"not found\"}. Paths are relative to \
                              the workspace root and use '/' separators. Use list_dir first if \
                              you do not know the exact path."
                    .into(),
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
                description: "Search your simulation workspace for a literal substring. Returns \
                              {\"pattern\":..., \"matches\":[{\"path\":..., \"line\":..., \
                              \"text\":...}], \"truncated\":bool} (at most 200 matches). The \
                              \"pattern\" is a LITERAL substring, not a regex. Use this to find \
                              where something is defined or referenced."
                    .into(),
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

/// Max lines one `read` call may return, to keep the simulator's context
/// bounded (its working set should scale with the trace, not the file).
const MAX_READ_LINES: usize = 2000;
/// Max matches one `grep` may return.
const MAX_GREP_MATCHES: usize = 200;
/// Max characters of a line included in grep output (long lines are
/// truncated with a marker).
const MAX_LINE_LEN: usize = 500;

/// Decompress a zip entirely in memory and return a workspace seeded with
/// its files. Nothing is written to disk. Hard caps (compressed and
/// decompressed) and zip-slip rejection make a malicious or malformed
/// archive safe: it cannot escape the workspace root or exhaust memory.
pub fn unpack_zip(bytes: &[u8]) -> Result<Workspace, WorkspaceError> {
    if bytes.len() > COMPRESSED_LIMIT {
        return Err(WorkspaceError::TooLargeCompressed {
            size: bytes.len(),
            limit: COMPRESSED_LIMIT,
        });
    }
    let cursor = std::io::Cursor::new(bytes);
    let mut archive =
        ZipArchive::new(cursor).map_err(|e| WorkspaceError::BadZip(e.to_string()))?;
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
        let path = normalize(&raw_name)
            .ok_or_else(|| WorkspaceError::PathTraversal(raw_name.clone()))?;
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
            if total_decompressed > DECOMPRESSED_LIMIT {
                return Err(WorkspaceError::TooLargeDecompressed {
                    size: total_decompressed,
                    limit: DECOMPRESSED_LIMIT,
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
    })
}

/// Normalize a path to a workspace-relative form and reject anything
/// that escapes the root. `'\'` is treated as a separator, a single
/// leading `'/'` is stripped (treated as relative to root), empty / `.`
/// components are dropped, and any `..` component (or a NUL byte) makes
/// the path invalid. The result never starts with `/` and never
/// contains `..`, so it cannot traverse above the workspace root — the
/// zip-slip guard.
fn normalize(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let stripped = raw.strip_prefix('/').unwrap_or(raw);
    // Accept backslash separators (Windows-style) by normalizing to '/'.
    let replaced = stripped.replace('\\', "/");
    if replaced.is_empty() {
        return None;
    }
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

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

fn usize_arg(args: &Value, key: &str) -> Option<usize> {
    args.get(key).and_then(|v| v.as_u64()).map(|n| n as usize)
}

fn bool_arg(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(|v| v.as_bool())
}

fn truncate_line(line: &str) -> String {
    if line.chars().count() <= MAX_LINE_LEN {
        return line.to_string();
    }
    let head: String = line.chars().take(MAX_LINE_LEN).collect();
    format!("{head}… <truncated>")
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
        }
    }

    #[test]
    fn normalize_rejects_traversal() {
        assert_eq!(normalize("src/main.rs").as_deref(), Some("src/main.rs"));
        assert_eq!(normalize("/src/main.rs").as_deref(), Some("src/main.rs"));
        assert_eq!(normalize("./src/./a.rs").as_deref(), Some("src/a.rs"));
        assert_eq!(normalize("src\\main.rs").as_deref(), Some("src/main.rs"));
        assert_eq!(normalize("../etc/passwd"), None);
        assert_eq!(normalize("a/../../b"), None);
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
        let big = (0..MAX_READ_LINES + 50)
            .map(|i| format!("l{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let w = ws(&[("big.txt", &big)]);
        let r = w.exec_read(&json!({"path": "big.txt"}));
        assert_eq!(r["end_line"], json!(MAX_READ_LINES));
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
    fn grep_finds_substrings() {
        let w = ws(&[
            ("src/a.rs", "fn alpha() {}\nfn beta() {}\n"),
            ("src/b.rs", "alpha used here\n"),
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
    fn unpack_zip_in_memory_roundtrip() {
        // Build a zip entirely in memory.
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts =
                zip::write::SimpleFileOptions::default();
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
        let bytes = vec![0u8; COMPRESSED_LIMIT + 1];
        assert!(matches!(
            unpack_zip(&bytes),
            Err(WorkspaceError::TooLargeCompressed { .. })
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
