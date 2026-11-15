//! Deterministic project detection.

use std::collections::BTreeMap;
use std::path::Path;

use crate::ToolOutput;

const MANIFESTS: &[(&str, &str)] = &[
    ("Cargo.toml", "Rust (Cargo)"),
    ("package.json", "JavaScript/TypeScript (npm)"),
    ("pyproject.toml", "Python (pyproject)"),
    ("requirements.txt", "Python (pip)"),
    ("setup.py", "Python (setuptools)"),
    ("go.mod", "Go modules"),
    ("pom.xml", "Java (Maven)"),
    ("build.gradle", "JVM (Gradle)"),
    ("build.gradle.kts", "JVM (Gradle)"),
    ("Gemfile", "Ruby (Bundler)"),
    ("composer.json", "PHP (Composer)"),
    ("Package.swift", "Swift (SwiftPM)"),
    ("CMakeLists.txt", "C/C++ (CMake)"),
    ("Makefile", "Make"),
    ("Dockerfile", "Docker"),
    ("mix.exs", "Elixir (Mix)"),
    ("deno.json", "Deno"),
];

fn lang(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "rs" => "Rust",
        "py" => "Python",
        "js" | "mjs" | "cjs" | "jsx" => "JavaScript",
        "ts" | "tsx" => "TypeScript",
        "go" => "Go",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "swift" => "Swift",
        "c" | "h" => "C",
        "cc" | "cpp" | "hpp" | "cxx" => "C++",
        "cs" => "C#",
        "rb" => "Ruby",
        "php" => "PHP",
        "ex" | "exs" => "Elixir",
        "sh" | "bash" | "zsh" => "Shell",
        "html" => "HTML",
        "css" | "scss" => "CSS",
        "md" => "Markdown",
        "sql" => "SQL",
        "lua" => "Lua",
        "zig" => "Zig",
        _ => return None,
    })
}

/// Best guess at the command that runs the test suite.
pub fn test_command(root: &Path) -> Option<String> {
    let has = |f: &str| root.join(f).exists();
    if has("Cargo.toml") {
        return Some("cargo test".into());
    }
    if has("package.json") {
        let pkg = std::fs::read_to_string(root.join("package.json")).ok()?;
        let v: serde_json::Value = serde_json::from_str(&pkg).ok()?;
        v.pointer("/scripts/test")?;
        let pm = if has("pnpm-lock.yaml") {
            "pnpm"
        } else if has("yarn.lock") {
            "yarn"
        } else if has("bun.lockb") || has("bun.lock") {
            "bun"
        } else {
            "npm"
        };
        return Some(format!("{pm} test"));
    }
    if has("go.mod") {
        return Some("go test ./...".into());
    }
    if has("pyproject.toml") || has("pytest.ini") || has("setup.py") || root.join("tests").is_dir() && has("requirements.txt") {
        return Some("python -m pytest -q".into());
    }
    if has("mix.exs") {
        return Some("mix test".into());
    }
    if has("Package.swift") {
        return Some("swift test".into());
    }
    if has("Makefile") && std::fs::read_to_string(root.join("Makefile")).is_ok_and(|m| m.lines().any(|l| l.starts_with("test:"))) {
        return Some("make test".into());
    }
    None
}

fn frameworks(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(v) =
        std::fs::read_to_string(root.join("package.json")).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
    {
        for key in ["dependencies", "devDependencies"] {
            if let Some(deps) = v.get(key).and_then(|d| d.as_object()) {
                for name in
                    ["react", "next", "vue", "svelte", "@angular/core", "express", "fastify", "vite", "typescript", "jest", "vitest"]
                {
                    if deps.contains_key(name) && !out.contains(&name.to_string()) {
                        out.push(name.to_string());
                    }
                }
            }
        }
    }
    let py = ["pyproject.toml", "requirements.txt"]
        .iter()
        .filter_map(|f| std::fs::read_to_string(root.join(f)).ok())
        .collect::<String>()
        .to_lowercase();
    for name in ["django", "flask", "fastapi", "pytest", "numpy", "pandas", "torch"] {
        if py.contains(name) {
            out.push(name.into());
        }
    }
    if let Ok(cargo) = std::fs::read_to_string(root.join("Cargo.toml")) {
        if cargo.contains("[workspace]") {
            out.push("cargo workspace".into());
        }
        for name in ["tokio", "axum", "actix-web", "ratatui", "serde", "clap"] {
            if cargo.contains(&format!("{name} ")) || cargo.contains(&format!("{name}=")) || cargo.contains(&format!("{name}.")) {
                out.push(name.into());
            }
        }
    }
    out
}

pub fn info(root: &Path) -> ToolOutput {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut files = 0usize;
    let walker = ignore::WalkBuilder::new(root).hidden(true).require_git(false).build();
    for e in walker.flatten().take(50_000) {
        if e.file_type().is_some_and(|t| t.is_file()) {
            files += 1;
            if let Some(l) = e.path().extension().and_then(|x| x.to_str()).and_then(lang) {
                *counts.entry(l).or_default() += 1;
            }
        }
    }
    let mut langs: Vec<_> = counts.into_iter().collect();
    langs.sort_by_key(|l| std::cmp::Reverse(l.1));
    let langs: Vec<String> = langs.iter().take(6).map(|(l, n)| format!("{l} ({n})")).collect();
    let manifests: Vec<String> = MANIFESTS.iter().filter(|(f, _)| root.join(f).exists()).map(|(f, d)| format!("{f} — {d}")).collect();
    let fw = frameworks(root);
    let git = if root.join(".git").exists() {
        let branch = std::process::Command::new("git").args(["branch", "--show-current"]).current_dir(root).output().ok();
        let dirty = std::process::Command::new("git").args(["status", "--porcelain"]).current_dir(root).output().ok();
        format!(
            "branch {} · {} changed file(s)",
            branch
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|b| !b.is_empty())
                .unwrap_or_else(|| "(detached)".into()),
            dirty.map_or(0, |o| String::from_utf8_lossy(&o.stdout).lines().count())
        )
    } else {
        "not a git repository".into()
    };
    let readme = ["README.md", "README", "readme.md", "README.rst"].iter().find(|f| root.join(f).exists()).copied();
    let mut text = format!("Project root: {}\nFiles: {files} (excluding ignored)\n", root.display());
    text.push_str(&format!("Languages: {}\n", if langs.is_empty() { "unknown".into() } else { langs.join(", ") }));
    text.push_str(&format!("Manifests: {}\n", if manifests.is_empty() { "none".into() } else { manifests.join("; ") }));
    if !fw.is_empty() {
        text.push_str(&format!("Frameworks/libraries: {}\n", fw.join(", ")));
    }
    text.push_str(&format!("Git: {git}\n"));
    text.push_str(&format!("Test command: {}\n", test_command(root).unwrap_or_else(|| "not detected".into())));
    if let Some(r) = readme {
        text.push_str(&format!("README: {r}\n"));
    }
    ToolOutput { ok: true, summary: langs.first().cloned().unwrap_or_else(|| format!("{files} files")), content: text }
}

/// A short project summary for the system prompt (no git calls, cheap).
pub fn brief(root: &Path) -> String {
    let manifests: Vec<&str> = MANIFESTS.iter().filter(|(f, _)| root.join(f).exists()).map(|(_, d)| *d).collect();
    let mut s = String::new();
    if !manifests.is_empty() {
        s.push_str(&format!("Detected: {}. ", manifests.join(", ")));
    }
    if let Some(t) = test_command(root) {
        s.push_str(&format!("Tests: `{t}`. "));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_projects_and_test_commands() {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        assert_eq!(test_command(r), None);
        std::fs::write(r.join("package.json"), r#"{"scripts":{"test":"vitest"},"devDependencies":{"vitest":"1","typescript":"5"}}"#)
            .unwrap();
        std::fs::write(r.join("pnpm-lock.yaml"), "").unwrap();
        std::fs::write(r.join("index.ts"), "").unwrap();
        assert_eq!(test_command(r).as_deref(), Some("pnpm test"));
        let out = info(r);
        assert!(out.content.contains("TypeScript (1)"));
        assert!(out.content.contains("vitest") && out.content.contains("typescript"));
        assert!(brief(r).contains("npm"));
        std::fs::write(r.join("Cargo.toml"), "[package]\nname='x'\n").unwrap();
        assert_eq!(test_command(r).as_deref(), Some("cargo test"));
    }
}
