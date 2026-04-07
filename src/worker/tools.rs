//! Agent tool sandbox — sandboxed filesystem tools for the agentic execution loop.
//!
//! When a kew agent runs, it can call these tools mid-generation to explore the
//! codebase, search for patterns, and write files. All operations are sandboxed
//! to the project root with path-traversal guards.
//!
//! ## Available tools
//!
//! | Tool         | Description                                            |
//! |--------------|--------------------------------------------------------|
//! | `read_file`  | Read a file with optional line range (100 KB cap)      |
//! | `list_dir`   | List directory contents with file types and sizes      |
//! | `grep`       | Regex search across files (configurable scope)         |
//! | `write_file` | Write content to a file (respects advisory locks)      |
//!
//! ## Security model
//!
//! All paths are resolved relative to a `project_root` and canonicalized. Any
//! path that escapes the project root after resolution is rejected. This is the
//! same guard used by the worker's path resolution.

use regex::Regex;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

use crate::llm::{ToolCall, ToolDefinition, ToolFunction};

// ToolCallFunction is used in tests
#[cfg(test)]
use crate::llm::ToolCallFunction;

/// Maximum bytes returned from a single `read_file` call.
const READ_FILE_MAX_BYTES: usize = 102_400; // 100 KB

/// Maximum number of grep matches returned.
const GREP_MAX_RESULTS: usize = 50;

/// Maximum number of directory entries returned.
const LIST_DIR_MAX_ENTRIES: usize = 200;

/// Maximum file size for write_file (1 MB).
const WRITE_FILE_MAX_BYTES: usize = 1_048_576;

/// Maximum iterations for the agentic tool loop before forcing a final answer.
pub const MAX_TOOL_ITERATIONS: usize = 25;

// --- Tool parameter types ---

#[derive(Deserialize)]
struct ReadFileParams {
    /// File path relative to the project root.
    path: String,
    /// Start line (1-indexed, inclusive). Omit to start from the beginning.
    start_line: Option<usize>,
    /// End line (1-indexed, inclusive). Omit to read to the end.
    end_line: Option<usize>,
}

#[derive(Deserialize)]
struct ListDirParams {
    /// Directory path relative to project root. Defaults to "." (project root).
    path: Option<String>,
}

#[derive(Deserialize)]
struct GrepParams {
    /// Regex pattern to search for.
    pattern: String,
    /// Directory or file path to search in (relative to project root). Defaults to ".".
    path: Option<String>,
    /// Glob pattern to filter files (e.g. "*.rs", "**/*.ts"). Defaults to all files.
    glob: Option<String>,
    /// Maximum number of matches to return. Defaults to 50.
    max_results: Option<usize>,
}

#[derive(Deserialize)]
struct WriteFileParams {
    /// File path relative to project root.
    path: String,
    /// Content to write.
    content: String,
}

// --- Tool sandbox ---

/// Sandboxed tool executor scoped to a project directory.
///
/// Every tool call is resolved against `project_root`. Paths that escape
/// it are rejected. The sandbox is created per-task execution and shared
/// across all tool calls within that task.
pub struct ToolSandbox {
    project_root: PathBuf,
    /// Task ID — used for write_file lock checking.
    task_id: String,
    /// Database handle for lock checking on writes.
    db: crate::db::Database,
}

impl ToolSandbox {
    pub fn new(project_root: PathBuf, task_id: String, db: crate::db::Database) -> Self {
        // Canonicalize the project root to resolve symlinks (e.g. macOS /var → /private/var).
        // This ensures `starts_with` checks work correctly against canonicalized child paths.
        let project_root = project_root.canonicalize().unwrap_or(project_root);
        Self {
            project_root,
            task_id,
            db,
        }
    }

    /// Return all tool definitions for the agentic loop to send to the LLM.
    pub fn definitions() -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                tool_type: "function".into(),
                function: ToolFunction {
                    name: "read_file".into(),
                    description:
                        "Read a file from the project. Returns the file content with \
                        line numbers. Paths are relative to the project root. \
                        Max 100 KB per read. Use start_line/end_line to read slices of large files."
                            .into(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "required": ["path"],
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "File path relative to the project root"
                            },
                            "start_line": {
                                "type": "integer",
                                "description": "Start line (1-indexed, inclusive). Omit to start from beginning."
                            },
                            "end_line": {
                                "type": "integer",
                                "description": "End line (1-indexed, inclusive). Omit to read to end."
                            }
                        }
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".into(),
                function: ToolFunction {
                    name: "list_dir".into(),
                    description: "List files and directories at a path. Returns names, types \
                        (file/dir/symlink), and sizes. Paths are relative to the project root. \
                        Defaults to the project root if path is omitted."
                        .into(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "Directory path relative to project root. Defaults to \".\" (root)."
                            }
                        }
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".into(),
                function: ToolFunction {
                    name: "grep".into(),
                    description:
                        "Search for a regex pattern across project files. Returns matching \
                        lines with file paths and line numbers. Use `path` to narrow the search \
                        scope and `glob` to filter by file extension."
                            .into(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "required": ["pattern"],
                        "properties": {
                            "pattern": {
                                "type": "string",
                                "description": "Regex pattern to search for"
                            },
                            "path": {
                                "type": "string",
                                "description": "Directory or file to search in (relative to project root). Defaults to \".\"."
                            },
                            "glob": {
                                "type": "string",
                                "description": "Glob pattern to filter files, e.g. \"*.rs\" or \"**/*.ts\""
                            },
                            "max_results": {
                                "type": "integer",
                                "description": "Maximum matches to return (default: 50)"
                            }
                        }
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".into(),
                function: ToolFunction {
                    name: "write_file".into(),
                    description: "Write content to a file in the project. Creates parent \
                        directories if needed. Overwrites existing files. Will fail if the file \
                        is locked by another task. Max 1 MB per write."
                        .into(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "required": ["path", "content"],
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "File path relative to project root"
                            },
                            "content": {
                                "type": "string",
                                "description": "Content to write to the file"
                            }
                        }
                    }),
                },
            },
        ]
    }

    /// Execute a tool call and return the result as a string.
    ///
    /// The returned string is the content of the tool result message that gets
    /// sent back to the LLM. On error, returns a human-readable error message
    /// (not a Rust error) so the LLM can adjust its approach.
    pub fn execute(&self, call: &ToolCall) -> String {
        let name = &call.function.name;
        let args = &call.function.arguments;

        debug!(tool = %name, "executing agent tool");

        match name.as_str() {
            "read_file" => self.exec_read_file(args),
            "list_dir" => self.exec_list_dir(args),
            "grep" => self.exec_grep(args),
            "write_file" => self.exec_write_file(args),
            _ => format!(
                "error: unknown tool '{name}'. Available: read_file, list_dir, grep, write_file"
            ),
        }
    }

    // --- Path resolution ---

    /// Resolve a relative path to an absolute path within the project root.
    /// Returns `None` if the path escapes the sandbox.
    fn resolve_path(&self, rel_path: &str) -> Option<PathBuf> {
        let candidate = self.project_root.join(rel_path);

        // For existing paths, canonicalize to resolve symlinks and ..
        if candidate.exists() {
            match candidate.canonicalize() {
                Ok(abs) if abs.starts_with(&self.project_root) => Some(abs),
                Ok(_) => {
                    warn!("path '{rel_path}' escapes project root after canonicalization");
                    None
                }
                Err(_) => None,
            }
        } else {
            // For new files (write_file), normalize without canonicalize.
            // Check that the resolved path doesn't contain .. components that escape.
            let normalized = normalize_path(&candidate);
            if normalized.starts_with(&self.project_root) {
                Some(normalized)
            } else {
                warn!("path '{rel_path}' escapes project root after normalization");
                None
            }
        }
    }

    // --- Tool implementations ---

    fn exec_read_file(&self, args: &serde_json::Value) -> String {
        let params: ReadFileParams = match serde_json::from_value(args.clone()) {
            Ok(p) => p,
            Err(e) => return format!("error: invalid parameters: {e}"),
        };

        let abs = match self.resolve_path(&params.path) {
            Some(p) => p,
            None => return format!("error: path '{}' is outside the project root", params.path),
        };

        let raw = match std::fs::read_to_string(&abs) {
            Ok(s) => s,
            Err(e) => return format!("error: cannot read '{}': {e}", params.path),
        };

        let lines: Vec<&str> = raw.lines().collect();
        let total = lines.len();

        // Apply line range (1-indexed)
        let start = params.start_line.map(|n| n.saturating_sub(1)).unwrap_or(0);
        let end = params.end_line.unwrap_or(total).min(total);
        let sliced = &lines[start.min(total)..end.min(total)];

        // Format with line numbers
        let mut output = String::new();
        for (i, line) in sliced.iter().enumerate() {
            let line_num = start + i + 1;
            output.push_str(&format!("{line_num:>5}\t{line}\n"));
        }

        // Apply byte cap
        if output.len() > READ_FILE_MAX_BYTES {
            let mut boundary = READ_FILE_MAX_BYTES;
            while boundary > 0 && !output.is_char_boundary(boundary) {
                boundary -= 1;
            }
            output.truncate(boundary);
            output.push_str(&format!(
                "\n... truncated at {READ_FILE_MAX_BYTES} bytes (file has {total} lines total)"
            ));
        }

        if start > 0 || end < total {
            output.push_str(&format!(
                "\n[showing lines {}-{} of {total}]",
                start + 1,
                end
            ));
        }

        output
    }

    fn exec_list_dir(&self, args: &serde_json::Value) -> String {
        let params: ListDirParams = match serde_json::from_value(args.clone()) {
            Ok(p) => p,
            Err(e) => return format!("error: invalid parameters: {e}"),
        };

        let rel = params.path.as_deref().unwrap_or(".");
        let abs = match self.resolve_path(rel) {
            Some(p) => p,
            None => return format!("error: path '{rel}' is outside the project root"),
        };

        if !abs.is_dir() {
            return format!("error: '{rel}' is not a directory");
        }

        let entries = match std::fs::read_dir(&abs) {
            Ok(e) => e,
            Err(e) => return format!("error: cannot read directory '{rel}': {e}"),
        };

        let mut items: Vec<String> = Vec::new();
        for entry in entries.flatten() {
            if items.len() >= LIST_DIR_MAX_ENTRIES {
                items.push(format!("... truncated at {LIST_DIR_MAX_ENTRIES} entries"));
                break;
            }

            let name = entry.file_name().to_string_lossy().to_string();

            // Skip hidden files and common noise
            if name.starts_with('.') && name != ".kew" {
                continue;
            }

            let meta = entry.metadata();
            let (kind, size) = match &meta {
                Ok(m) if m.is_dir() => ("dir", String::new()),
                Ok(m) if m.is_symlink() => ("link", String::new()),
                Ok(m) => ("file", format_size(m.len())),
                Err(_) => ("?", String::new()),
            };

            if size.is_empty() {
                items.push(format!("{kind:>4}  {name}/"));
            } else {
                items.push(format!("{kind:>4}  {name}  ({size})"));
            }
        }

        items.sort();

        if items.is_empty() {
            format!("(empty directory: {rel})")
        } else {
            items.join("\n")
        }
    }

    fn exec_grep(&self, args: &serde_json::Value) -> String {
        let params: GrepParams = match serde_json::from_value(args.clone()) {
            Ok(p) => p,
            Err(e) => return format!("error: invalid parameters: {e}"),
        };

        let re = match Regex::new(&params.pattern) {
            Ok(r) => r,
            Err(e) => return format!("error: invalid regex '{}': {e}", params.pattern),
        };

        let search_root = match self.resolve_path(params.path.as_deref().unwrap_or(".")) {
            Some(p) => p,
            None => return "error: search path is outside the project root".into(),
        };

        let glob_pattern = params.glob.as_deref();
        let max = params
            .max_results
            .unwrap_or(GREP_MAX_RESULTS)
            .min(GREP_MAX_RESULTS);

        let mut matches = Vec::new();
        walk_files(&search_root, &self.project_root, &mut |path| {
            if matches.len() >= max {
                return;
            }

            // Apply glob filter
            if let Some(glob) = glob_pattern {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                if !simple_glob_match(glob, &name) {
                    return;
                }
            }

            // Skip binary files (simple heuristic: check first 512 bytes for null)
            let Ok(content) = std::fs::read_to_string(path) else {
                return;
            };

            let rel = path
                .strip_prefix(&self.project_root)
                .unwrap_or(path)
                .to_string_lossy();

            for (i, line) in content.lines().enumerate() {
                if matches.len() >= max {
                    break;
                }
                if re.is_match(line) {
                    matches.push(format!(
                        "{}:{}: {}",
                        rel,
                        i + 1,
                        line.chars().take(200).collect::<String>()
                    ));
                }
            }
        });

        if matches.is_empty() {
            format!("no matches found for pattern '{}'", params.pattern)
        } else {
            let count = matches.len();
            let truncated = if count >= max {
                format!("\n... showing first {max} matches")
            } else {
                String::new()
            };
            format!("{}{truncated}", matches.join("\n"))
        }
    }

    fn exec_write_file(&self, args: &serde_json::Value) -> String {
        let params: WriteFileParams = match serde_json::from_value(args.clone()) {
            Ok(p) => p,
            Err(e) => return format!("error: invalid parameters: {e}"),
        };

        if params.content.len() > WRITE_FILE_MAX_BYTES {
            return format!(
                "error: content size ({} bytes) exceeds maximum ({WRITE_FILE_MAX_BYTES} bytes)",
                params.content.len()
            );
        }

        let abs = match self.resolve_path(&params.path) {
            Some(p) => p,
            None => return format!("error: path '{}' is outside the project root", params.path),
        };

        // Check advisory lock — is another task holding a lock on this file?
        {
            let conn = self.db.conn();
            match crate::db::locks::check_lock(&conn, &params.path) {
                Ok(Some(holder)) if holder != self.task_id => {
                    return format!("error: file '{}' is locked by task {holder}", params.path);
                }
                Err(e) => {
                    warn!("lock check failed for '{}': {e}", params.path);
                    // Proceed — lock check is advisory, not fatal
                }
                _ => {} // unlocked or locked by us
            }
        }

        // Create parent directories if needed
        if let Some(parent) = abs.parent() {
            if !parent.exists() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    return format!("error: cannot create directory: {e}");
                }
            }
        }

        match std::fs::write(&abs, &params.content) {
            Ok(()) => format!("wrote {} bytes to '{}'", params.content.len(), params.path),
            Err(e) => format!("error: cannot write '{}': {e}", params.path),
        }
    }
}

// --- Utility functions ---

/// Normalize a path without requiring it to exist (no symlink resolution).
/// Collapses `.` and `..` components lexically.
fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                components.pop();
            }
            std::path::Component::CurDir => {}
            other => components.push(other),
        }
    }
    components.iter().collect()
}

/// Recursively walk files under `root`, calling `callback` for each regular file.
/// Skips: hidden directories (except `.kew`), `target/`, `node_modules/`, `.git/`.
fn walk_files(root: &Path, project_root: &Path, callback: &mut impl FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        // Skip noise directories
        if path.is_dir() {
            if name == ".git"
                || name == "target"
                || name == "node_modules"
                || name == ".build"
                || name == "dist"
                || (name.starts_with('.') && name != ".kew")
            {
                continue;
            }
            // Guard: don't escape project root via symlinks
            if let Ok(canonical) = path.canonicalize() {
                if canonical.starts_with(project_root) {
                    walk_files(&path, project_root, callback);
                }
            }
        } else if path.is_file() {
            callback(&path);
        }
    }
}

/// Simple glob matching — supports `*` (any chars) and `?` (single char).
/// Matches against the filename only, not the full path.
fn simple_glob_match(pattern: &str, name: &str) -> bool {
    // Handle common case: "*.ext"
    if let Some(ext) = pattern.strip_prefix("*.") {
        return name.ends_with(&format!(".{ext}"));
    }
    // Handle "**/*.ext" — strip the prefix and match extension
    if let Some(rest) = pattern.strip_prefix("**/") {
        return simple_glob_match(rest, name);
    }
    // Fallback: exact match or contains
    pattern == name || pattern == "*"
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn test_sandbox(dir: &Path) -> ToolSandbox {
        let db = Database::open_in_memory().unwrap();
        ToolSandbox::new(dir.to_path_buf(), "test-task".into(), db)
    }

    #[test]
    fn test_definitions_are_valid_json() {
        let defs = ToolSandbox::definitions();
        assert_eq!(defs.len(), 4);
        for def in &defs {
            assert_eq!(def.tool_type, "function");
            assert!(!def.function.name.is_empty());
            assert!(!def.function.description.is_empty());
            // Parameters should be a valid JSON object with "type": "object"
            assert_eq!(def.function.parameters["type"], "object");
        }
    }

    #[test]
    fn test_read_file_basic() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("hello.txt");
        std::fs::write(&file_path, "line 1\nline 2\nline 3\n").unwrap();

        let sandbox = test_sandbox(dir.path());
        let call = ToolCall {
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "read_file".into(),
                arguments: serde_json::json!({"path": "hello.txt"}),
            },
        };

        let result = sandbox.execute(&call);
        assert!(result.contains("line 1"));
        assert!(result.contains("line 2"));
        assert!(result.contains("line 3"));
    }

    #[test]
    fn test_read_file_line_range() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("lines.txt");
        let content: String = (1..=10).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&file_path, &content).unwrap();

        let sandbox = test_sandbox(dir.path());
        let call = ToolCall {
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "read_file".into(),
                arguments: serde_json::json!({"path": "lines.txt", "start_line": 3, "end_line": 5}),
            },
        };

        let result = sandbox.execute(&call);
        assert!(result.contains("line 3"));
        assert!(result.contains("line 5"));
        assert!(!result.contains("line 1"));
        assert!(!result.contains("line 6"));
        assert!(result.contains("showing lines 3-5 of 10"));
    }

    #[test]
    fn test_read_file_path_traversal_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = test_sandbox(dir.path());
        let call = ToolCall {
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "read_file".into(),
                arguments: serde_json::json!({"path": "../../etc/passwd"}),
            },
        };

        let result = sandbox.execute(&call);
        assert!(result.contains("error"));
        assert!(!result.contains("root:"));
    }

    #[test]
    fn test_list_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "// lib").unwrap();

        let sandbox = test_sandbox(dir.path());
        let call = ToolCall {
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "list_dir".into(),
                arguments: serde_json::json!({}),
            },
        };

        let result = sandbox.execute(&call);
        assert!(result.contains("main.rs"));
        assert!(result.contains("src/"));
    }

    #[test]
    fn test_grep_basic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("code.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("other.rs"),
            "fn other() {\n    let x = 42;\n}\n",
        )
        .unwrap();

        let sandbox = test_sandbox(dir.path());
        let call = ToolCall {
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "grep".into(),
                arguments: serde_json::json!({"pattern": "fn main"}),
            },
        };

        let result = sandbox.execute(&call);
        assert!(result.contains("code.rs:1:"));
        assert!(result.contains("fn main"));
        assert!(!result.contains("fn other"));
    }

    #[test]
    fn test_grep_with_glob() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("app.rs"), "fn rust_code() {}\n").unwrap();
        std::fs::write(dir.path().join("app.py"), "def python_code():\n    pass\n").unwrap();

        let sandbox = test_sandbox(dir.path());
        let call = ToolCall {
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "grep".into(),
                arguments: serde_json::json!({"pattern": "code", "glob": "*.rs"}),
            },
        };

        let result = sandbox.execute(&call);
        assert!(result.contains("rust_code"));
        assert!(!result.contains("python_code"));
    }

    #[test]
    fn test_grep_no_matches() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("empty.rs"), "// nothing here\n").unwrap();

        let sandbox = test_sandbox(dir.path());
        let call = ToolCall {
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "grep".into(),
                arguments: serde_json::json!({"pattern": "nonexistent_pattern_xyz"}),
            },
        };

        let result = sandbox.execute(&call);
        assert!(result.contains("no matches found"));
    }

    #[test]
    fn test_write_file_basic() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = test_sandbox(dir.path());
        let call = ToolCall {
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "write_file".into(),
                arguments: serde_json::json!({"path": "new.txt", "content": "hello world"}),
            },
        };

        let result = sandbox.execute(&call);
        assert!(result.contains("wrote 11 bytes"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
            "hello world"
        );
    }

    #[test]
    fn test_write_file_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = test_sandbox(dir.path());
        let call = ToolCall {
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "write_file".into(),
                arguments: serde_json::json!({"path": "deep/nested/file.txt", "content": "nested"}),
            },
        };

        let result = sandbox.execute(&call);
        assert!(result.contains("wrote"));
        assert!(dir.path().join("deep/nested/file.txt").exists());
    }

    #[test]
    fn test_write_file_path_traversal_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = test_sandbox(dir.path());
        let call = ToolCall {
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "write_file".into(),
                arguments: serde_json::json!({"path": "../escape.txt", "content": "bad"}),
            },
        };

        let result = sandbox.execute(&call);
        assert!(result.contains("error"));
    }

    #[test]
    fn test_write_file_respects_lock() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open_in_memory().unwrap();

        // Create a task and lock a file
        {
            let conn = db.conn();
            let new = crate::db::models::NewTask {
                model: "mock".into(),
                provider: crate::db::models::Provider::Ollama,
                prompt: "blocker".into(),
                system_prompt: None,
                context_keys: vec![],
                share_as: None,
                files_locked: vec![],
                parent_id: None,
                chain_id: None,
                chain_index: None,
            };
            crate::db::tasks::create_task(&conn, &new).unwrap();
            let blocker = crate::db::tasks::claim_next_pending(&conn, "w0")
                .unwrap()
                .unwrap();
            crate::db::locks::acquire_lock(&conn, "locked.txt", &blocker.id, 600).unwrap();
        }

        let sandbox = ToolSandbox::new(dir.path().to_path_buf(), "different-task".into(), db);
        let call = ToolCall {
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "write_file".into(),
                arguments: serde_json::json!({"path": "locked.txt", "content": "attempt"}),
            },
        };

        let result = sandbox.execute(&call);
        assert!(result.contains("locked by task"));
    }

    #[test]
    fn test_unknown_tool() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = test_sandbox(dir.path());
        let call = ToolCall {
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "hack_mainframe".into(),
                arguments: serde_json::json!({}),
            },
        };

        let result = sandbox.execute(&call);
        assert!(result.contains("unknown tool"));
    }

    #[test]
    fn test_simple_glob_match() {
        assert!(simple_glob_match("*.rs", "main.rs"));
        assert!(!simple_glob_match("*.rs", "main.py"));
        assert!(simple_glob_match("**/*.rs", "main.rs"));
        assert!(simple_glob_match("*", "anything"));
        assert!(simple_glob_match("main.rs", "main.rs"));
        assert!(!simple_glob_match("main.rs", "lib.rs"));
    }

    #[test]
    fn test_normalize_path() {
        let p = normalize_path(Path::new("/a/b/../c/./d"));
        assert_eq!(p, PathBuf::from("/a/c/d"));
    }
}
