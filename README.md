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

> Status: early development (v0.1). Local chat, model management, providers
> and the terminal UI work today. Agentic coding tools (file editing, shell
> commands) and persistent sessions are the next milestones; see
> [docs/PROGRESS.md](docs/PROGRESS.md).

## Install

Only building from source is available right now. Homebrew, release binaries
and other channels are planned but **not yet published**.

```bash
git clone <this repository> 212 && cd 212
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
| `/models` | installed models (Enter load · `i` info · `Del` remove) |
| `/models search <q>` · `pull <spec>` · `import <path\|ollama[:name]>` · `remove` · `info` · `switch` · `cancel` | model management |
| `/recommend` | models suited to this machine |
| `/provider [local\|ollama\|openai\|anthropic\|compat] [model]` | switch provider without restarting |
| `/theme [name]` | theme picker with live preview, or switch directly |
| `/animations on\|reduced\|off` | animation level |
| `/mode chat\|plan\|build` | agent mode (tools arrive in a later milestone) |
| `/new` · `/clear` | new conversation · clear the view |
| `/context` · `/status` · `/doctor` | context usage · runtime status · diagnostics |
| `/copy [code]` | copy the last reply or its last code block (OSC 52, works over SSH) |
| `/unload` | stop the local model and free its memory |
| `/backend` | install the verified llama.cpp backend |
| `/help` · `/quit` | |

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

API keys are read from environment variables and never written to logs or the
config file.

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
requests only when you search, download, install the backend, or choose a
cloud provider.

## Documentation

- [Architecture and design decisions](docs/ARCHITECTURE.md)
- [Configuration reference](docs/CONFIGURATION.md)
- [Progress, known limitations, benchmarks](docs/PROGRESS.md)

MIT licensed.
