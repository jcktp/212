//! Slash commands: registry, parsing and completion.

pub struct CommandSpec {
    pub name: &'static str,
    pub args: &'static str,
    pub help: &'static str,
}

pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec { name: "help", args: "", help: "Show commands and keys" },
    CommandSpec { name: "models", args: "[search|pull|import|remove|info|switch|cancel] …", help: "Model manager" },
    CommandSpec { name: "model", args: "[id]", help: "Show the active model, or switch to one" },
    CommandSpec { name: "recommend", args: "", help: "Models suited to this machine" },
    CommandSpec { name: "provider", args: "[local|ollama|openai|anthropic|compat] [model]", help: "Switch AI provider" },
    CommandSpec { name: "theme", args: "[name]", help: "Pick a color theme (live preview)" },
    CommandSpec { name: "animations", args: "on|reduced|off", help: "Configure animations" },
    CommandSpec { name: "mode", args: "chat|plan|build", help: "Change agent mode" },
    CommandSpec { name: "new", args: "", help: "Start a new conversation" },
    CommandSpec { name: "clear", args: "", help: "Clear the conversation view" },
    CommandSpec { name: "context", args: "", help: "Show context usage" },
    CommandSpec { name: "status", args: "", help: "Show runtime status" },
    CommandSpec { name: "doctor", args: "", help: "Run diagnostics" },
    CommandSpec { name: "copy", args: "[code]", help: "Copy the last reply (or its code) via OSC 52" },
    CommandSpec { name: "unload", args: "", help: "Stop the local model and free memory" },
    CommandSpec { name: "backend", args: "install", help: "Install the verified llama.cpp backend" },
    CommandSpec { name: "settings", args: "", help: "Show where settings live" },
    CommandSpec { name: "quit", args: "", help: "Exit 212" },
];

#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub name: String,
    pub args: Vec<String>,
    pub rest: String,
}

/// Parse `/name args…`. Returns `None` for ordinary messages. `//text` is an
/// escape for sending a message that starts with a slash.
pub fn parse(input: &str) -> Option<Parsed> {
    let s = input.trim();
    let body = s.strip_prefix('/')?;
    if body.starts_with('/') || body.is_empty() {
        return None;
    }
    let (name, rest) = match body.split_once(char::is_whitespace) {
        Some((n, r)) => (n, r.trim()),
        None => (body, ""),
    };
    let name = name.to_lowercase();
    let name = match name.as_str() {
        "q" | "exit" => "quit".to_string(),
        "h" | "?" => "help".to_string(),
        _ => name,
    };
    Some(Parsed { name, args: rest.split_whitespace().map(str::to_string).collect(), rest: rest.to_string() })
}

/// Commands whose name starts with `prefix` (without the slash).
pub fn complete(prefix: &str) -> Vec<&'static CommandSpec> {
    let p = prefix.trim_start_matches('/').to_lowercase();
    COMMANDS.iter().filter(|c| c.name.starts_with(&p)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        let p = parse("/models pull ollama:qwen3").unwrap();
        assert_eq!(p.name, "models");
        assert_eq!(p.args, vec!["pull", "ollama:qwen3"]);
        assert_eq!(p.rest, "pull ollama:qwen3");
        assert_eq!(parse("/Q").unwrap().name, "quit");
        assert!(parse("hello").is_none());
        assert!(parse("//not a command").is_none());
        assert!(parse("/").is_none());
    }

    #[test]
    fn completes_prefixes() {
        let names: Vec<_> = complete("/mo").iter().map(|c| c.name).collect();
        assert_eq!(names, vec!["models", "model", "mode"]);
        assert_eq!(complete("").len(), COMMANDS.len());
        assert!(complete("/zzz").is_empty());
    }
}
