//! Workspace confinement.

use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}

/// Files that commonly hold credentials. Tools refuse to read or write them.
pub fn is_sensitive(p: &Path) -> bool {
    let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    (name == ".env" || name.starts_with(".env."))
        && !name.ends_with(".example")
        && !name.ends_with(".sample")
        && !name.ends_with(".template")
        || name.starts_with("id_rsa")
        || name.starts_with("id_ed25519")
        || name.starts_with("id_ecdsa")
        || matches!(name.as_str(), ".netrc" | ".npmrc" | ".pypirc" | ".git-credentials")
        || name.contains("credentials")
        || matches!(ext.as_str(), "pem" | "key" | "p12" | "pfx" | "keystore" | "jks")
        || p.components().any(|c| matches!(c.as_os_str().to_str(), Some(".ssh" | ".aws" | ".gnupg")))
}

impl Workspace {
    pub fn new(root: &Path) -> std::io::Result<Self> {
        Ok(Self { root: std::fs::canonicalize(root)? })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn rel(&self, abs: &Path) -> String {
        let r = abs.strip_prefix(&self.root).unwrap_or(abs).to_string_lossy().replace('\\', "/");
        if r.is_empty() { ".".into() } else { r }
    }

    /// Normalize without touching the filesystem. Rejects anything that would
    /// leave the workspace (absolute paths elsewhere, `..` above the root).
    pub fn resolve_lexical(&self, input: &str) -> Result<(PathBuf, String), String> {
        let input = input.trim();
        if input.is_empty() {
            return Err("path must not be empty".into());
        }
        let p = Path::new(input);
        let rel_part = if p.is_absolute() {
            p.strip_prefix(&self.root).map_err(|_| format!("{input} is outside the workspace ({})", self.root.display()))?.to_path_buf()
        } else {
            p.to_path_buf()
        };
        let mut out = self.root.clone();
        let depth0 = self.root.components().count();
        for c in rel_part.components() {
            match c {
                Component::CurDir => {}
                Component::ParentDir => {
                    if out.components().count() <= depth0 {
                        return Err(format!("{input} is outside the workspace"));
                    }
                    out.pop();
                }
                Component::Normal(s) => out.push(s),
                Component::RootDir | Component::Prefix(_) => return Err(format!("{input} is outside the workspace")),
            }
        }
        let rel = self.rel(&out);
        Ok((out, rel))
    }

    /// Resolve a path that must exist, following symlinks, inside the workspace.
    pub fn resolve_existing(&self, input: &str) -> Result<(PathBuf, String), String> {
        let (lex, rel) = self.resolve_lexical(input)?;
        let real = std::fs::canonicalize(&lex).map_err(|_| format!("{rel}: no such file or directory"))?;
        if !real.starts_with(&self.root) {
            return Err(format!("{rel} resolves outside the workspace"));
        }
        Ok((real, rel))
    }

    /// Resolve a path to create or overwrite.
    pub fn resolve_for_write(&self, input: &str) -> Result<(PathBuf, String), String> {
        let (lex, rel) = self.resolve_lexical(input)?;
        if lex == self.root {
            return Err("cannot write to the workspace root itself".into());
        }
        // The deepest existing ancestor (or the file itself) must really be inside.
        let mut probe = lex.clone();
        while !probe.exists() {
            if !probe.pop() {
                break;
            }
        }
        let real = std::fs::canonicalize(&probe).map_err(|e| format!("{rel}: {e}"))?;
        if !real.starts_with(&self.root) {
            return Err(format!("{rel} resolves outside the workspace"));
        }
        let target = if lex.exists() { std::fs::canonicalize(&lex).map_err(|e| e.to_string())? } else { lex };
        if target.is_dir() {
            return Err(format!("{rel} is a directory"));
        }
        if !self.writable(&target) {
            return Err(format!("{rel} is protected (git internals or a credential file) and cannot be written"));
        }
        Ok((target, rel))
    }

    /// Is this (resolved) path allowed to be modified?
    pub fn writable(&self, p: &Path) -> bool {
        let rel = p.strip_prefix(&self.root).unwrap_or(p);
        let in_git = rel.components().next().is_some_and(|c| c.as_os_str() == ".git");
        !in_git && !is_sensitive(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_resolution_stays_inside() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = Workspace::new(tmp.path()).unwrap();
        assert_eq!(ws.resolve_lexical("src/../a.txt").unwrap().1, "a.txt");
        assert_eq!(ws.resolve_lexical("./").unwrap().1, ".");
        assert!(ws.resolve_lexical("../x").is_err());
        assert!(ws.resolve_lexical("a/../../x").is_err());
        assert!(ws.resolve_lexical("/etc/passwd").is_err());
        let inside = ws.root().join("b.txt");
        assert_eq!(ws.resolve_lexical(inside.to_str().unwrap()).unwrap().1, "b.txt");
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_cannot_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("ws");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(tmp.path().join("outside")).unwrap();
        std::fs::write(tmp.path().join("outside/f"), "x").unwrap();
        std::os::unix::fs::symlink(tmp.path().join("outside"), root.join("link")).unwrap();
        let ws = Workspace::new(&root).unwrap();
        assert!(ws.resolve_existing("link/f").unwrap_err().contains("outside"));
        assert!(ws.resolve_for_write("link/new.txt").unwrap_err().contains("outside"));
    }

    #[test]
    fn protected_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
        let ws = Workspace::new(tmp.path()).unwrap();
        assert!(ws.resolve_for_write(".git/config").is_err());
        assert!(ws.resolve_for_write(".env").is_err());
        assert!(ws.resolve_for_write("certs/server.key").is_err());
        assert!(ws.resolve_for_write(".env.example").is_ok());
        assert!(ws.resolve_for_write("src/new/deep/file.rs").is_ok());
        assert!(ws.resolve_for_write(".").is_err());
    }
}
