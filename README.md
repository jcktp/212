```
██████    ██    ██████
  ░░██░░████░░    ░░██░░
██████░░  ██░░  ██████░░
██░░░░░░  ██░░  ██░░░░░░
██████  ██████  ██████
  ░░░░░░  ░░░░░░  ░░░░░░
```

# 212

**A lightweight, retro-styled, local-first AI terminal assistant.**

212 is one native executable that chats with language models running on your
own machine. It manages its own [llama.cpp](https://github.com/ggml-org/llama.cpp)
inference backend, finds and downloads GGUF models from Hugging Face or the
Ollama registry, and wraps it all in a fast 8/16-bit-inspired terminal
interface with eight color themes. Ollama, Docker and model servers are **not**
required. Cloud providers (OpenAI, Anthropic, any OpenAI-compatible endpoint)
are optional.

> Status: early development (v0.1). Local chat, model management, providers,
> the coding agent (read, search, edit, run commands and tests, with
> approval), saved sessions, web links and Agent Skills work today; see
> [docs/PROGRESS.md](docs/PROGRESS.md).

## Install

Only building from source is available right now. Homebrew, release binaries
and other channels are planned but **not yet published**.

```bash
git clone https://github.com/jcktp/212.git && cd 212
cargo install --path crates/app
```

That puts `212` in `~/.cargo/bin`. You need a Rust toolchain (1.85+).

### Inference backend

212 looks for `llama-server` in this order:

1. `inference.server_path` in the config file
2. a backend that 212 installed itself
3. `llama-server` on your `PATH` (e.g. `brew install llama.cpp`)

If none is found, 212 offers to download the official prebuilt release from
`github.com/ggml-org/llama.cpp`. It shows the file, size and SHA-256 first, and
installs only after you confirm and the checksum matches. You can also run it
directly:

```bash
212 backend
```

## Quick start

```bash
212
```

On first launch 212 shows a short boot sequence, lets you pick a theme (with
live preview), detects your hardware, and recommends models that fit. Models
you already have in a local Ollama install are imported by hard link, with no
download and no Ollama process needed.

Headless use:

```bash
212 -p "explain this error" < build.log      # one-shot, streams to stdout
212 models recommend
212 models pull Qwen/Qwen3-4B-GGUF:Q4_K_M     # asks before downloading
212 models pull ollama:qwen3:4b
212 models import ~/Downloads/model.gguf
212 models import ollama:llama3.2              # from a local Ollama store
212 doctor
```

## Using it

| Key | Action |
| --- | --- |
| `Enter` | send |
| `Alt+Enter` / `Ctrl+J` / `Shift+Enter`¹ | new line |
| `↑` `↓` | input history (or move between lines) |
| `Tab` | complete `/commands` and `@files` |
| `Esc` | stop generation, model loading or a download |
| `PgUp` `PgDn`, `Shift+↑↓`, `Ctrl+End` | scroll the conversation |
| `Ctrl+T` | theme picker |
| `Ctrl+O` | show/hide model reasoning |
| `Ctrl+C` | stop → clear input → quit |

¹ Shift+Enter works in terminals with the kitty keyboard protocol (kitty, Ghostty, WezTerm, recent iTerm2).

Type `@path/to/file` to attach a file from the current project. Only files
inside the working directory can be attached. Files that look like secrets
(`.env`, keys, `.ssh/`) are refused, and large files are truncated.

### Commands

| Command | |
| --- | --- |
| `/models` | one list of installed models, models already on disk (Ollama) and recommended downloads. Enter switches to any of them, importing or downloading first (downloads ask). `i` info · `Ctrl+D` remove (asks) |
| `/models search <q>` · `pull <spec>` · `import <path\|ollama[:name]>` · `remove` · `info` · `switch` · `cancel` | model management |
| `/recommend` | models suited to this machine |
| `/provider [local\|ollama\|openai\|anthropic\|compat] [model]` | switch provider without restarting |
| `/theme [name]` | theme picker with live preview, or switch directly |
| `/animations on\|reduced\|off` | animation level |
| `/mode chat\|plan\|build` | agent mode (see below) |
| `/new` · `/clear` | new conversation · clear the view |
| `/sessions [all]` · `/resume [id]` · `/rename <title>` | browse, resume and name saved conversations (`212 -c` continues the last one) |
| `/history <words>` | full-text search across all saved conversations |
| `/export [md\|json] [path]` | write the conversation to a file |
| `/compact` | summarize older messages to free context (also automatic when the window fills) |
| `/key <provider>` · `/key remove <provider>` | store an API key in the system keychain (masked input, never in history) |
| `/skills [add <git URL\|owner/repo\|folder>]` · `/skill <name>` | list or install Agent Skills · activate one |
| `/weather <place>\|off` | weather in the header (opt-in, via open-meteo.com) |
| `/context` · `/status` · `/doctor` | context usage · runtime status · diagnostics |
| `/copy [code]` | copy the last reply or its last code block (OSC 52, works over SSH) |
| `/unload` | stop the local model and free its memory |
| `/backend` | install the verified llama.cpp backend |
| `/help` · `/quit` | |

### Agent modes

| Mode | What the model can do |
| --- | --- |
| `chat` (default) | talk only. No tools. |
| `plan` | read, list, find, grep, inspect the project and git history. Read-only. Ends with a plan. |
| `build` | everything in plan, plus edit/create files, run commands and run tests |

In build mode every edit shows a diff and every command shows its exact
command line before anything happens. You choose **Allow once**, **Allow for
this session** (all edits, or that exact command) or **Deny**. Commands that
look risky (`rm -r`, `sudo`, `git push`, piping downloads into a shell…) always
ask and can't be allowed for the whole session. Press `Esc` to stop the agent
at any point; a running command is killed together with its child processes.
`Ctrl+O` expands full diffs, command output and reasoning.

Start in a mode with `212 --mode build`. Tools need a model whose chat
template supports them (shown in `/models info`); larger models do much better.

What the agent **cannot** do: access files outside the directory you started
212 in, read credential files (`.env`, keys, `.ssh/`…), write inside `.git/`,
or see your API keys (they are removed from command environments). Commands
run with your user's permissions. There is no OS-level sandbox, so read the
command before you allow it. Every tool call is logged to `logs/audit.jsonl`.

### Sessions

Every conversation is saved automatically to a local SQLite database, grouped
by the directory you started 212 in. Resume the last one with `212 -c` (or
`/resume`), browse with `/sessions`, search everything with `/history`, and
export to Markdown or JSON with `/export`. When the context window fills, older
turns are summarized automatically (`/compact` does it on demand). The full
transcript always stays in the database. Set `sessions.autosave = false` to
keep nothing.

### Links and the web

Put `@` before a link to include the page in your message:
`summarize @https://example.com/post`. The model can also read pages itself
with the `web_fetch` tool (in every mode). By default it asks first, per site,
with "allow this site for the session". Pages are converted to text. Local and
private-network addresses are refused. Set `agent.web_access` to `allow` or
`off`.

### Skills

212 supports the open **Agent Skills** format: a folder with a `SKILL.md`
whose frontmatter has `name` and `description`, plus optional scripts and
references. Install with `/skills add owner/repo`, a git URL or a local folder.
Skills are also picked up from `.agents/skills/`, `.claude/skills/` and
`.212/skills/` in your project. The model sees each skill's name and
description and loads the full instructions with `load_skill` when relevant.
`/skill <name>` forces one on for the conversation. Installing never runs code,
and scripts a skill mentions still need your approval in build mode.

### Header

The top line shows the model, provider, CPU and memory (with the model
server's share) and, if you turn it on with `/weather <place>`, the current
weather. Disable the stats with `ui.system_stats = false`.

### Themes

`phosphor` · `amber` · `dos` · `arcade` · `gameboy` · `cyber` · `paper` · `monochrome`

Themes switch instantly and degrade from truecolor to 256, 16 or no color
automatically (`NO_COLOR` is respected). Custom themes are TOML files; see
[docs/CONFIGURATION.md](docs/CONFIGURATION.md#custom-themes) and
[assets/themes/midnight-teal.toml](assets/themes/midnight-teal.toml).

### Providers

| Provider | Needs |
| --- | --- |
| `local` (default) | a GGUF model; 212 runs llama.cpp itself |
| `ollama` | a running Ollama server (optional integration) |
| `openai` | `OPENAI_API_KEY` |
| `anthropic` | `ANTHROPIC_API_KEY` |
| `compat` | `providers.compat_url` (LM Studio, vLLM, a remote llama-server…) |

API keys come from environment variables or the system keychain (`/key
anthropic`, or `212 key set anthropic`). They are never written to logs, the
config file or saved sessions, and they are removed from the environment of
commands the agent runs.

## Where things live

Run `212 paths`. Defaults:

| | macOS | Linux | Windows |
| --- | --- | --- | --- |
| Config | `~/Library/Application Support/212/config.toml` | `~/.config/212/config.toml` | `%APPDATA%\212\config\config.toml` |
| Models | `~/Library/Application Support/212/models/` | `~/.local/share/212/models/` | `%APPDATA%\212\data\models\` |
| Backend | `…/212/backends/llama.cpp/` | `~/.local/share/212/backends/` | `%APPDATA%\212\data\backends\` |
| Logs | `…/212/logs/` | `~/.local/share/212/logs/` | `%APPDATA%\212\data\logs\` |

Set `TWO_ONE_TWO_HOME=/some/dir` to keep everything in one place.

## Update and uninstall

```bash
git pull && cargo install --path crates/app      # update
cargo uninstall two-one-two                       # remove the binary
```

Then delete the directories listed by `212 paths` if you also want to remove
configuration, downloaded models and the managed backend. Models imported by
hard link from elsewhere (e.g. Ollama) keep their original copy.

## Privacy

Everything runs locally by default. The local backend listens on `127.0.0.1`
only, on a random port, behind a random per-session API key. 212 makes network
requests only when you search, download, install the backend or a skill, use a
cloud provider, include or approve a web link, or enable the header weather
(which sends only the place name to open-meteo.com).

## Documentation

- [Architecture and design decisions](docs/ARCHITECTURE.md)
- [Configuration reference](docs/CONFIGURATION.md)
- [Progress, known limitations, benchmarks](docs/PROGRESS.md)

MIT licensed.
