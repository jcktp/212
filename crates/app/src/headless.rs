//! Non-interactive operation: one-shot prompts and model commands.
//! Output is plain text; no colors or animations.

use std::io::{BufRead, Write};
use std::time::Duration;

use common::StreamEvent;
use common::format;
use common::proto::{DownloadPhase, Event, InferenceState, NoticeLevel, ReportStatus, Request};
use tokio::sync::mpsc;

pub enum Job {
    Prompt(String),
    Request(Request),
    Doctor,
    Nothing,
}

/// Returns Ok(false) when the job failed (the reason was already printed).
pub async fn run(job: Job, yes: bool, tx: mpsc::Sender<Request>, mut rx: mpsc::Receiver<Event>) -> anyhow::Result<bool> {
    let mut out = std::io::stdout();
    let mut err = std::io::stderr();
    let mut failed = false;
    let mut sent = false;
    let mut waiting_ready = matches!(job, Job::Prompt(_));
    let is_prompt = waiting_ready;
    let mut prompt = match job {
        Job::Prompt(p) => Some(p),
        Job::Request(r) => {
            tx.send(r).await?;
            sent = true;
            None
        }
        Job::Doctor => {
            tx.send(Request::Doctor { terminal: vec![] }).await?;
            sent = true;
            None
        }
        Job::Nothing => return Ok(true),
    };
    let mut last_progress = std::time::Instant::now();
    let mut active_downloads = false;
    let mut pending_choice = false;
    let mut in_reasoning = false;
    loop {
        let _ = (sent, pending_choice);
        let ev = match tokio::time::timeout(Duration::from_secs(600), rx.recv()).await {
            Ok(Some(ev)) => ev,
            Ok(None) => break,
            Err(_) => {
                writeln!(err, "error: no response for 10 minutes")?;
                failed = true;
                break;
            }
        };
        match ev {
            Event::Inference(InferenceState::Ready) if waiting_ready => {
                waiting_ready = false;
                if let Some(p) = prompt.take() {
                    tx.send(Request::Send { text: p }).await?;
                    sent = true;
                }
            }
            Event::Inference(InferenceState::Offline) if waiting_ready => {
                writeln!(err, "No model is loaded. Import or pull one first (212 models --help), or pass --model.")?;
                failed = true;
                break;
            }
            Event::Inference(InferenceState::Error(e)) => {
                writeln!(err, "error: {e}")?;
                failed = true;
                break;
            }
            Event::Delta(StreamEvent::Text(t)) => {
                if in_reasoning {
                    writeln!(err)?;
                    in_reasoning = false;
                }
                write!(out, "{t}")?;
                out.flush()?;
            }
            Event::Delta(StreamEvent::Reasoning(r)) => {
                // Reasoning goes to stderr so stdout stays clean for piping.
                in_reasoning = true;
                write!(err, "{r}")?;
            }
            Event::TurnEnd { completion_tokens, tokens_per_second, cancelled, .. } => {
                writeln!(out)?;
                let speed = tokens_per_second.map(|t| format!(", {t:.1} tok/s")).unwrap_or_default();
                writeln!(err, "[{completion_tokens} tokens{speed}{}]", if cancelled { ", cancelled" } else { "" })?;
                break;
            }
            Event::Notice { level, text } => {
                let tag = match level {
                    NoticeLevel::Info => "info",
                    NoticeLevel::Success => "ok",
                    NoticeLevel::Warn => "warning",
                    NoticeLevel::Error => {
                        failed = true;
                        "error"
                    }
                };
                writeln!(err, "{tag}: {text}")?;
                // A success or error notice concludes model/backend commands.
                if (!is_prompt || level == NoticeLevel::Error)
                    && matches!(level, NoticeLevel::Success | NoticeLevel::Error)
                    && !active_downloads
                {
                    break;
                }
            }
            Event::Models(list) => {
                let (installed, available): (Vec<_>, Vec<_>) = list.into_iter().partition(|m| m.installed);
                if installed.is_empty() {
                    writeln!(out, "No models installed.")?;
                }
                for m in &installed {
                    writeln!(
                        out,
                        "{}{:<36} {:<8} {:>9}  {}  {}",
                        if m.active { "* " } else { "  " },
                        m.id,
                        m.quant,
                        format::bytes(m.size),
                        m.arch,
                        m.source
                    )?;
                }
                if !available.is_empty() {
                    writeln!(out, "\nAvailable (212 models pull <spec>):")?;
                    for m in &available {
                        writeln!(out, "  {:<44} {}", m.install_spec.as_deref().unwrap_or(&m.id), m.source)?;
                    }
                }
                break;
            }
            Event::SearchResults { query, hits } => {
                writeln!(out, "{query}:")?;
                for h in hits {
                    writeln!(out, "  {:<48} {}", h.spec, h.detail)?;
                }
                break;
            }
            Event::SessionList { title, items } => {
                writeln!(out, "{title}:")?;
                for i in items {
                    writeln!(out, "  {}{:<16} {:<44} {}", if i.current { "* " } else { "  " }, i.id, i.title, i.detail)?;
                }
                break;
            }
            Event::Report { title, lines, .. } => {
                writeln!(out, "{title}")?;
                for l in lines {
                    let icon = match l.status {
                        ReportStatus::Ok => "ok  ",
                        ReportStatus::Warn => "warn",
                        ReportStatus::Fail => {
                            failed = true;
                            "FAIL"
                        }
                        ReportStatus::Info => "    ",
                    };
                    writeln!(out, "  [{icon}] {:<14} {}", l.label, l.detail)?;
                }
                break;
            }
            Event::Choice { id, title, body, options, preview } => {
                writeln!(err, "{title}")?;
                for b in &body {
                    writeln!(err, "  {b}")?;
                }
                if let Some(p) = &preview {
                    for l in p.lines().take(60) {
                        writeln!(err, "    {l}")?;
                    }
                }
                // Tool permissions are never auto-approved by --yes; they need a person.
                let is_permission = preview.is_some();
                let deny_index = if is_permission { options.len() - 1 } else { 1 };
                let accept = if yes && !is_permission {
                    true
                } else if options.len() >= 2 && std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                    write!(err, "{}? [y/N] ", options[0])?;
                    err.flush()?;
                    let mut line = String::new();
                    std::io::stdin().lock().read_line(&mut line)?;
                    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
                } else if is_permission {
                    writeln!(err, "(denied: tool actions need confirmation on an interactive terminal)")?;
                    false
                } else {
                    writeln!(err, "(not confirmed; re-run with --yes to accept)")?;
                    false
                };
                pending_choice = accept;
                tx.send(Request::Answer { id, choice: Some(if accept { 0 } else { deny_index }) }).await?;
                if !accept && !is_permission {
                    break;
                }
            }
            Event::ToolStart { title, .. } => {
                writeln!(err, "\n[tool] {title}")?;
            }
            Event::ToolEnd { ok, summary, .. } => {
                writeln!(err, "[tool] {} {summary}", if ok { "ok" } else { "failed" })?;
            }
            Event::Download(p) => {
                pending_choice = false;
                match p.phase {
                    DownloadPhase::Finished | DownloadPhase::Failed | DownloadPhase::Cancelled => {
                        active_downloads = false;
                        writeln!(err)?;
                    }
                    _ => {
                        active_downloads = true;
                        if last_progress.elapsed() > Duration::from_millis(500) {
                            last_progress = std::time::Instant::now();
                            let pct = p
                                .total
                                .filter(|t| *t > 0)
                                .map(|t| format!("{:>5.1}%", p.downloaded as f64 / t as f64 * 100.0))
                                .unwrap_or_default();
                            write!(
                                err,
                                "\r{} {pct} {} @ {}/s   ",
                                p.label,
                                format::bytes(p.downloaded),
                                format::bytes(p.bytes_per_sec as u64)
                            )?;
                            err.flush()?;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(!failed)
}
