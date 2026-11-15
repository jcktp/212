//! File system tools.

use std::path::Path;

use crate::ToolOutput;

const MAX_READ_BYTES: u64 = 1024 * 1024;
const MAX_ENTRIES: usize = 400;
const MAX_MATCHES: usize = 200;

pub fn unified_diff(rel: &str, old: &str, new: &str) -> String {
    let d = similar::TextDiff::from_lines(old, new);
    let body = d.unified_diff().context_radius(3).header(&format!("a/{rel}"), &format!("b/{rel}")).to_string();
    if body.is_empty() { format!("(no changes to {rel})") } else { body }
}

/// Replace `old` with `new`. `old` must occur exactly once unless `all`.
pub fn apply_edit(content: &str, old: &str, new: &str, all: bool) -> Result<String, String> {
    if old.is_empty() {
        return Err("old_string must not be empty (use write_file to create files)".into());
    }
    if old == new {
        return Err("old_string and new_string are identical".into());
    }
    let n = content.matches(old).count();
    match (n, all) {
        (0, _) => {
            // Help the model recover from whitespace mismatches.
            let hint =
                if content.contains(old.trim()) { " (it matches if surrounding whitespace is ignored — copy it exactly)" } else { "" };
            Err(format!("old_string was not found in the file{hint}. Read the file again and copy the text exactly."))
        }
        (1, _) | (_, true) => Ok(content.replace(old, new)),
        (n, false) => Err(format!("old_string occurs {n} times; include more context to make it unique, or set replace_all")),
    }
}

pub fn read(path: &Path, rel: &str, offset: usize, limit: usize) -> ToolOutput {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => return ToolOutput::error(format!("{rel}: {e}")),
    };
    if meta.is_dir() {
        return ToolOutput::error(format!("{rel} is a directory; use list_dir"));
    }
    if meta.len() > MAX_READ_BYTES * 8 {
        return ToolOutput::error(format!("{rel} is too large ({} bytes); use grep to find the relevant part", meta.len()));
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return ToolOutput::error(format!("{rel}: {e}")),
    };
    if bytes.iter().take(8192).any(|b| *b == 0) {
        return ToolOutput::error(format!("{rel} is a binary file"));
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    if total == 0 {
        return ToolOutput::ok(format!("{rel} is empty."), "empty file".into());
    }
    let start = (offset - 1).min(total);
    let end = (start + limit).min(total);
    let mut body = lines[start..end].join("\n");
    let header = if start == 0 && end == total {
        format!("{rel} ({total} lines):\n")
    } else {
        format!("{rel} lines {}-{} of {total}:\n", start + 1, end)
    };
    if end < total {
        body.push_str(&format!("\n… {} more lines; read again with offset={}", total - end, end + 1));
    }
    ToolOutput::ok(format!("{header}{body}"), format!("{} lines", end - start))
}

fn walker(root: &Path, dir: &Path) -> ignore::WalkBuilder {
    let mut w = ignore::WalkBuilder::new(dir);
    w.hidden(false).git_ignore(true).git_exclude(true).parents(true).require_git(false);
    let _ = root;
    w.filter_entry(|e| e.file_name() != ".git");
    w
}

pub fn list(root: &Path, dir: &Path, rel: &str, depth: usize) -> ToolOutput {
    if !dir.is_dir() {
        return ToolOutput::error(format!("{rel} is not a directory"));
    }
    let mut entries = Vec::new();
    let mut truncated = false;
    for e in walker(root, dir).max_depth(Some(depth)).sort_by_file_name(|a, b| a.cmp(b)).build().flatten() {
        if e.depth() == 0 {
            continue;
        }
        if entries.len() >= MAX_ENTRIES {
            truncated = true;
            break;
        }
        let p = e.path().strip_prefix(root).unwrap_or(e.path()).to_string_lossy().replace('\\', "/");
        let indent = "  ".repeat(e.depth() - 1);
        let name = e.file_name().to_string_lossy();
        entries.push(if e.file_type().is_some_and(|t| t.is_dir()) { format!("{indent}{name}/") } else { format!("{indent}{name}") });
        let _ = p;
    }
    let n = entries.len();
    let mut out = format!("{rel}/\n{}", entries.join("\n"));
    if truncated {
        out.push_str(&format!("\n… (more than {MAX_ENTRIES} entries; list a subdirectory)"));
    }
    ToolOutput::ok(out, format!("{n} entries"))
}

pub fn find(root: &Path, pattern: &str) -> ToolOutput {
    let is_glob = pattern.contains(['*', '?', '[', '{']);
    let matcher = if is_glob {
        let g = if pattern.contains('/') { pattern.to_string() } else { format!("**/{pattern}") };
        match globset::Glob::new(&g) {
            Ok(g) => Some(g.compile_matcher()),
            Err(e) => return ToolOutput::error(format!("invalid glob: {e}")),
        }
    } else {
        None
    };
    let needle = pattern.to_lowercase();
    let mut hits = Vec::new();
    for e in walker(root, root).build().flatten() {
        if !e.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let rel = e.path().strip_prefix(root).unwrap_or(e.path()).to_string_lossy().replace('\\', "/");
        let ok = match &matcher {
            Some(m) => m.is_match(&rel),
            None => rel.to_lowercase().contains(&needle),
        };
        if ok {
            hits.push(rel);
            if hits.len() >= MAX_MATCHES {
                break;
            }
        }
    }
    hits.sort();
    let n = hits.len();
    if n == 0 {
        return ToolOutput::ok(format!("No files match '{pattern}'."), "no matches".into());
    }
    let more = if n >= MAX_MATCHES { "\n… (results truncated)" } else { "" };
    ToolOutput::ok(format!("{}{more}", hits.join("\n")), format!("{n} files"))
}

pub fn grep(root: &Path, pattern: &str, path: &Path, glob: Option<&str>, ci: bool) -> ToolOutput {
    let re = match regex::RegexBuilder::new(pattern).case_insensitive(ci).build() {
        Ok(r) => r,
        Err(e) => return ToolOutput::error(format!("invalid regex: {e}")),
    };
    let globm = match glob.map(|g| globset::Glob::new(if g.contains('/') { g.to_string() } else { format!("**/{g}") }.as_str())) {
        Some(Ok(g)) => Some(g.compile_matcher()),
        Some(Err(e)) => return ToolOutput::error(format!("invalid glob: {e}")),
        None => None,
    };
    let mut out = Vec::new();
    let mut files = 0;
    'walk: for e in walker(root, path).build().flatten() {
        if !e.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let rel = e.path().strip_prefix(root).unwrap_or(e.path()).to_string_lossy().replace('\\', "/");
        if globm.as_ref().is_some_and(|g| !g.is_match(&rel)) || crate::is_sensitive(e.path()) {
            continue;
        }
        let Ok(bytes) = std::fs::read(e.path()) else { continue };
        if bytes.len() as u64 > MAX_READ_BYTES || bytes.iter().take(8192).any(|b| *b == 0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let mut hit = false;
        for (i, line) in text.lines().enumerate() {
            if re.is_match(line) {
                hit = true;
                let l: String = line.trim_end().chars().take(200).collect();
                out.push(format!("{rel}:{}: {l}", i + 1));
                if out.len() >= MAX_MATCHES {
                    files += 1;
                    break 'walk;
                }
            }
        }
        if hit {
            files += 1;
        }
    }
    if out.is_empty() {
        return ToolOutput::ok(format!("No matches for /{pattern}/."), "no matches".into());
    }
    let n = out.len();
    let more = if n >= MAX_MATCHES { "\n… (more matches; narrow the search)" } else { "" };
    ToolOutput::ok(format!("{}{more}", out.join("\n")), format!("{n} matches in {files} files"))
}

fn atomic_write(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = path.with_file_name(format!(".{}.212tmp", path.file_name().unwrap_or_default().to_string_lossy()));
    std::fs::write(&tmp, content)?;
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, path)
}

fn changed_since_plan(path: &Path, expected: Option<&str>) -> bool {
    let now = std::fs::read_to_string(path).ok();
    now.as_deref() != expected
}

pub fn write(path: &Path, rel: &str, content: &str, original: Option<&str>) -> ToolOutput {
    if changed_since_plan(path, original) {
        return ToolOutput::error(format!("{rel} changed after the preview was made; read it again before writing"));
    }
    match atomic_write(path, content) {
        Ok(()) => {
            let lines = content.lines().count();
            let verb = if original.is_some() { "Updated" } else { "Created" };
            ToolOutput::ok(format!("{verb} {rel} ({lines} lines)."), format!("{} · {lines} lines", verb.to_lowercase()))
        }
        Err(e) => ToolOutput::error(format!("{rel}: {e}")),
    }
}

pub fn edit(path: &Path, rel: &str, original: &str, updated: &str) -> ToolOutput {
    if changed_since_plan(path, Some(original)) {
        return ToolOutput::error(format!("{rel} changed after the preview was made; read it again and retry"));
    }
    match atomic_write(path, updated) {
        Ok(()) => {
            let d = similar::TextDiff::from_lines(original, updated);
            let (mut add, mut del) = (0, 0);
            for c in d.iter_all_changes() {
                match c.tag() {
                    similar::ChangeTag::Insert => add += 1,
                    similar::ChangeTag::Delete => del += 1,
                    similar::ChangeTag::Equal => {}
                }
            }
            ToolOutput::ok(format!("Edited {rel} (+{add} -{del})."), format!("+{add} -{del}"))
        }
        Err(e) => ToolOutput::error(format!("{rel}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use crate::tests::ctx;
    use crate::{ToolKind, execute, plan};

    #[test]
    fn edit_requires_unique_exact_match() {
        assert!(super::apply_edit("a b a", "a", "c", false).unwrap_err().contains("2 times"));
        assert_eq!(super::apply_edit("a b a", "a", "c", true).unwrap(), "c b c");
        assert!(super::apply_edit("x  y", "x y", "z", false).unwrap_err().contains("not found"));
        assert!(super::apply_edit("abc", "", "z", false).is_err());
        assert_eq!(super::apply_edit("fn a() {}\n", "a()", "b()", false).unwrap(), "fn b() {}\n");
    }

    #[tokio::test]
    async fn read_list_find_grep() {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        std::fs::create_dir_all(r.join("src")).unwrap();
        std::fs::create_dir_all(r.join("target")).unwrap();
        std::fs::write(r.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(r.join("src/main.rs"), "fn main() {\n    println!(\"hi\");\n}\n").unwrap();
        std::fs::write(r.join("src/lib.rs"), (1..=50).map(|i| format!("line {i}\n")).collect::<String>()).unwrap();
        std::fs::write(r.join("target/junk.rs"), "fn main() {}").unwrap();
        std::fs::write(r.join(".env"), "SECRET=1").unwrap();
        let c = ctx(r);

        let out = execute(&plan("read_file", r#"{"path":"src/lib.rs","offset":10,"limit":3}"#, &c).unwrap(), &c).await;
        assert!(out.content.starts_with("src/lib.rs lines 10-12 of 50:\nline 10\nline 11\nline 12"), "{}", out.content);
        assert!(out.content.contains("offset=13"));

        let out = execute(&plan("list_dir", "{}", &c).unwrap(), &c).await;
        assert!(out.content.contains("src/") && out.content.contains("main.rs"));
        assert!(!out.content.contains("junk.rs"), "gitignored files are hidden");

        let out = execute(&plan("find_files", r#"{"pattern":"*.rs"}"#, &c).unwrap(), &c).await;
        assert_eq!(out.summary, "2 files");
        let out = execute(&plan("find_files", r#"{"pattern":"MAIN"}"#, &c).unwrap(), &c).await;
        assert_eq!(out.content, "src/main.rs");

        let out = execute(&plan("grep", r#"{"pattern":"println|SECRET"}"#, &c).unwrap(), &c).await;
        assert_eq!(out.content, "src/main.rs:2:     println!(\"hi\");");
        assert!(plan("read_file", r#"{"path":".env"}"#, &c).unwrap_err().contains("secret"));
        assert!(plan("read_file", r#"{"path":"../../etc/passwd"}"#, &c).unwrap_err().contains("outside"));
    }

    #[tokio::test]
    async fn write_and_edit_show_diff_and_detect_staleness() {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        let c = ctx(r);
        let p = plan("write_file", r#"{"path":"notes/todo.md","content":"- one\n- two\n"}"#, &c).unwrap();
        assert_eq!(p.kind, ToolKind::Write);
        assert!(p.title.starts_with("create"));
        assert!(p.preview.as_ref().unwrap().contains("+- two"));
        assert!(!r.join("notes/todo.md").exists(), "planning must not write");
        let out = execute(&p, &c).await;
        assert!(out.ok, "{}", out.content);
        assert_eq!(std::fs::read_to_string(r.join("notes/todo.md")).unwrap(), "- one\n- two\n");

        let p = plan("edit_file", r#"{"path":"notes/todo.md","old_string":"- two","new_string":"- 2"}"#, &c).unwrap();
        assert!(p.preview.as_ref().unwrap().contains("-- two\n+- 2"));
        // Someone else modifies the file between preview and execution.
        std::fs::write(r.join("notes/todo.md"), "- one\n- two\n- three\n").unwrap();
        let out = execute(&p, &c).await;
        assert!(!out.ok && out.content.contains("changed after the preview"));

        let p = plan("edit_file", r#"{"path":"notes/todo.md","old_string":"- two","new_string":"- 2"}"#, &c).unwrap();
        let out = execute(&p, &c).await;
        assert_eq!(out.summary, "+1 -1");
        assert_eq!(std::fs::read_to_string(r.join("notes/todo.md")).unwrap(), "- one\n- 2\n- three\n");
    }
}
