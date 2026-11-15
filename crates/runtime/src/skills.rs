//! Agent Skills (the open `SKILL.md` format).
//!
//! A skill is a folder with a `SKILL.md` file whose YAML frontmatter has a
//! `name` and a `description`, followed by Markdown instructions, optionally
//! with scripts and reference files beside it. Only names and descriptions go
//! into the system prompt; the model loads a skill's full text with the
//! `load_skill` tool when it needs it (or the user activates it with `/skill`).
//!
//! Skills are looked up in:
//! * `<config dir>/skills/` (installed with `/skills add`)
//! * `<project>/.212/skills/`, `<project>/.agents/skills/`, `<project>/.claude/skills/`
//!
//! Installing a skill never runs anything. Scripts a skill mentions run only
//! through `run_command`, which needs the user's approval.

use std::path::{Path, PathBuf};

use common::proto::{Event, NoticeLevel, ReportLine, ReportStatus};

use crate::{Internal, Runtime};

#[derive(Debug, Clone, PartialEq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub dir: PathBuf,
    pub origin: &'static str,
}

/// Parse `---\nname: x\ndescription: y\n---` frontmatter (simple YAML subset:
/// scalar values, optionally quoted, `>`/`|` folded blocks).
pub fn parse_frontmatter(text: &str) -> Option<(String, String)> {
    let rest = text.trim_start_matches('\u{feff}').strip_prefix("---")?;
    let end = rest.find("\n---")?;
    let block = &rest[..end];
    let mut name = None;
    let mut desc = None;
    let lines: Vec<&str> = block.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some((k, v)) = line.split_once(':')
            && !line.starts_with(' ')
        {
            let key = k.trim();
            let mut val = v.trim().to_string();
            if val == ">" || val == "|" || val == ">-" || val == "|-" {
                let mut parts = Vec::new();
                while i + 1 < lines.len() && (lines[i + 1].starts_with(' ') || lines[i + 1].trim().is_empty()) {
                    i += 1;
                    parts.push(lines[i].trim());
                }
                val = parts.join(" ").trim().to_string();
            }
            let val = val.trim_matches('"').trim_matches('\'').to_string();
            match key {
                "name" => name = Some(val),
                "description" => desc = Some(val),
                _ => {}
            }
        }
        i += 1;
    }
    let name = name.filter(|n| !n.is_empty())?;
    Some((name, desc.unwrap_or_default()))
}

fn scan(dir: &Path, origin: &'static str, out: &mut Vec<Skill>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.join("SKILL.md").is_file()).collect();
    dirs.sort();
    for d in dirs {
        let Ok(text) = std::fs::read_to_string(d.join("SKILL.md")) else { continue };
        let (name, description) =
            parse_frontmatter(&text).unwrap_or_else(|| (d.file_name().unwrap_or_default().to_string_lossy().into_owned(), String::new()));
        // Project skills override user skills with the same name.
        out.retain(|s: &Skill| !s.name.eq_ignore_ascii_case(&name));
        out.push(Skill { name, description, dir: d, origin });
    }
}

pub fn discover(user_dir: &Path, project: &Path) -> Vec<Skill> {
    let mut out = Vec::new();
    scan(user_dir, "installed", &mut out);
    for sub in [".claude/skills", ".agents/skills", ".212/skills"] {
        scan(&project.join(sub), "project", &mut out);
    }
    out
}

/// Find skill folders (containing SKILL.md) up to three levels below `root`.
pub fn find_skill_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if root.join("SKILL.md").is_file() {
        out.push(root.to_path_buf());
        return out;
    }
    for e in ignore::WalkBuilder::new(root).max_depth(Some(4)).hidden(true).build().flatten() {
        if e.file_name() == "SKILL.md"
            && let Some(p) = e.path().parent()
        {
            out.push(p.to_path_buf());
        }
    }
    out.sort();
    out.dedup();
    out
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)?.flatten() {
        let ty = e.file_type()?;
        let name = e.file_name();
        if name == ".git" || ty.is_symlink() {
            continue;
        }
        if ty.is_dir() {
            copy_dir(&e.path(), &dst.join(&name))?;
        } else {
            std::fs::copy(e.path(), dst.join(&name))?;
        }
    }
    Ok(())
}

/// Copy the skills under `src` into `dest_root`. Returns installed names.
pub fn install_from_dir(src: &Path, dest_root: &Path) -> Result<Vec<String>, String> {
    let dirs = find_skill_dirs(src);
    if dirs.is_empty() {
        return Err(format!("no SKILL.md found under {}", src.display()));
    }
    let mut names = Vec::new();
    for d in dirs {
        let text = std::fs::read_to_string(d.join("SKILL.md")).map_err(|e| e.to_string())?;
        let name =
            parse_frontmatter(&text).map(|(n, _)| n).unwrap_or_else(|| d.file_name().unwrap_or_default().to_string_lossy().into_owned());
        let folder = models::store::slug(&name);
        let dest = dest_root.join(&folder);
        if dest.exists() {
            std::fs::remove_dir_all(&dest).map_err(|e| e.to_string())?;
        }
        copy_dir(&d, &dest).map_err(|e| format!("copying {}: {e}", d.display()))?;
        names.push(name);
    }
    Ok(names)
}

impl Runtime {
    pub(crate) fn skills_dir(&self) -> PathBuf {
        self.paths.config_dir.join("skills")
    }

    pub(crate) fn refresh_skills(&mut self) {
        self.skills = discover(&self.skills_dir(), &self.cwd);
    }

    pub(crate) fn skill_refs(&self) -> Vec<tools::SkillRef> {
        self.skills.iter().map(|s| tools::SkillRef { name: s.name.clone(), dir: s.dir.clone() }).collect()
    }

    /// System-prompt section: the catalog plus any activated skills in full.
    pub(crate) fn skills_prompt(&self, can_load: bool) -> String {
        let mut s = String::new();
        if !self.skills.is_empty() && can_load {
            s.push_str("\n\nInstalled skills (call load_skill with the name before doing a task one of them covers):\n");
            for k in &self.skills {
                s.push_str(&format!("- {}: {}\n", k.name, k.description));
            }
        }
        for name in &self.active_skills {
            if let Some(k) = self.skills.iter().find(|k| &k.name == name)
                && let Ok(text) = std::fs::read_to_string(k.dir.join("SKILL.md"))
            {
                s.push_str(&format!(
                    "\n\nActive skill '{}' (files in {}):\n{}",
                    k.name,
                    k.dir.display(),
                    tools::truncate_middle(&text, 12_000)
                ));
            }
        }
        s
    }

    pub(crate) async fn list_skills(&mut self) {
        self.refresh_skills();
        if self.skills.is_empty() {
            return self
                .notice(
                    NoticeLevel::Info,
                    format!(
                        "No skills installed. Add some with /skills add <git URL or folder>, or put skill folders (each with a SKILL.md) in {} or in this project's .agents/skills/.",
                        self.skills_dir().display()
                    ),
                )
                .await;
        }
        let lines = self
            .skills
            .iter()
            .map(|k| ReportLine {
                status: if self.active_skills.contains(&k.name) { ReportStatus::Ok } else { ReportStatus::Info },
                label: k.name.clone(),
                detail: format!("{} · {}", if k.description.is_empty() { "(no description)" } else { &k.description }, k.origin),
            })
            .collect();
        self.emit(Event::Report {
            title: "Skills · /skill <name> activates one · the model can also load them itself".into(),
            art: None,
            lines,
        })
        .await;
    }

    pub(crate) async fn use_skill(&mut self, name: &str) {
        self.refresh_skills();
        let Some(k) = self.skills.iter().find(|k| k.name.eq_ignore_ascii_case(name.trim())).cloned() else {
            return self.notice(NoticeLevel::Error, format!("No skill named '{name}'. /skills lists them.")).await;
        };
        if self.active_skills.contains(&k.name) {
            self.active_skills.retain(|n| n != &k.name);
            return self.notice(NoticeLevel::Info, format!("Skill '{}' deactivated.", k.name)).await;
        }
        self.active_skills.push(k.name.clone());
        self.notice(
            NoticeLevel::Success,
            format!("Skill '{}' is active for this conversation (/skill {} again to turn it off).", k.name, k.name),
        )
        .await;
    }

    pub(crate) async fn install_skill(&mut self, source: &str) {
        let source = source.trim();
        if source.is_empty() {
            return self.notice(NoticeLevel::Info, "Usage: /skills add <git URL | owner/repo | local folder>").await;
        }
        let dest = self.skills_dir();
        let local = {
            let p = PathBuf::from(source.trim_start_matches("~/"));
            let p = if source.starts_with("~/") { std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(p) } else { p };
            if p.is_absolute() { p } else { self.cwd.join(p) }
        };
        let tx = self.internal_tx.clone();
        if local.is_dir() {
            let result = install_from_dir(&local, &dest);
            return self.on_skills_installed(result).await;
        }
        // owner/repo shorthand → GitHub.
        let url = if source.starts_with("https://") || source.starts_with("git@") {
            source.to_string()
        } else if source.split('/').count() == 2 && !source.contains(' ') {
            format!("https://github.com/{source}")
        } else {
            return self.notice(NoticeLevel::Error, format!("'{source}' is not a folder, a git URL or owner/repo.")).await;
        };
        self.notice(NoticeLevel::Info, format!("Cloning {url}…")).await;
        let tmp = self.paths.cache_dir.join(format!("skill-clone-{}", std::process::id()));
        tokio::spawn(async move {
            let _ = std::fs::remove_dir_all(&tmp);
            let out = tokio::process::Command::new("git")
                .args(["clone", "--depth", "1", "--quiet", &url])
                .arg(&tmp)
                .env("GIT_TERMINAL_PROMPT", "0")
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true)
                .output();
            let result = match tokio::time::timeout(std::time::Duration::from_secs(120), out).await {
                Err(_) => Err("git clone timed out".to_string()),
                Ok(Err(e)) => Err(format!("could not run git: {e}")),
                Ok(Ok(o)) if !o.status.success() => Err(format!("git clone failed: {}", String::from_utf8_lossy(&o.stderr).trim())),
                Ok(Ok(_)) => install_from_dir(&tmp, &dest),
            };
            let _ = std::fs::remove_dir_all(&tmp);
            let _ = tx.send(Internal::SkillsInstalled { result }).await;
        });
    }

    pub(crate) async fn on_skills_installed(&mut self, result: Result<Vec<String>, String>) {
        match result {
            Ok(names) => {
                self.refresh_skills();
                self.notice(
                    NoticeLevel::Success,
                    format!(
                        "Installed {} skill(s): {}. Skills can ask the model to run their scripts; commands still need your approval.",
                        names.len(),
                        names.join(", ")
                    ),
                )
                .await;
            }
            Err(e) => self.notice(NoticeLevel::Error, format!("Skill install failed: {e}")).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SKILL: &str = "---\nname: pdf-tools\ndescription: >\n  Work with PDF files:\n  extract text and merge.\nlicense: MIT\n---\n\n# PDF tools\nUse scripts/extract.py.\n";

    #[test]
    fn parses_frontmatter() {
        assert_eq!(parse_frontmatter(SKILL), Some(("pdf-tools".into(), "Work with PDF files: extract text and merge.".into())));
        assert_eq!(parse_frontmatter("---\nname: \"x\"\ndescription: 'y'\n---\n"), Some(("x".into(), "y".into())));
        assert_eq!(parse_frontmatter("# no frontmatter"), None);
        assert_eq!(parse_frontmatter("---\ndescription: d\n---\n"), None);
    }

    #[test]
    fn discovers_installs_and_overrides() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("repo/skills/pdf");
        std::fs::create_dir_all(src.join("scripts")).unwrap();
        std::fs::write(src.join("SKILL.md"), SKILL).unwrap();
        std::fs::write(src.join("scripts/extract.py"), "print(1)").unwrap();
        let user = tmp.path().join("user-skills");
        let names = install_from_dir(&tmp.path().join("repo"), &user).unwrap();
        assert_eq!(names, vec!["pdf-tools"]);
        assert!(user.join("pdf-tools/scripts/extract.py").exists());

        let project = tmp.path().join("proj");
        let p_skill = project.join(".agents/skills/pdf");
        std::fs::create_dir_all(&p_skill).unwrap();
        std::fs::write(p_skill.join("SKILL.md"), "---\nname: PDF-tools\ndescription: project version\n---\n").unwrap();
        let found = discover(&user, &project);
        assert_eq!(found.len(), 1, "project skill overrides the user skill");
        assert_eq!(found[0].description, "project version");
        assert!(install_from_dir(&tmp.path().join("nothing"), &user).is_err());
    }
}
