# 212 architecture

## Goals that shape the design

1. **One install, local AI that works.** No Ollama, Docker or manual server setup.
2. **Lightweight.** The UI should cost almost nothing next to the model it drives.
3. **Never block the terminal.** Typing, scrolling and cancelling stay responsive while models load, generate or download.
4. **Honest.** No faked output, progress, or capability claims.
5. **Room to grow** into a full coding agent without rewriting the core.

## Stack evaluation

The proposed stack (Rust, Ratatui, Crossterm, Tokio, llama.cpp, Reqwest, Serde, TOML, SQLite, tracing) was adopted with these decisions:

| Choice | Decision | Why / tradeoff |
| --- | --- | --- |
| Rust + Tokio | adopted | Native binary, predictable memory, good async story for streaming and process supervision. Cost: longer compile times. |
| Ratatui + Crossterm | adopted (0.30 / 0.29) | Mature, cross-platform, diff-based rendering. Crossterm works on Windows consoles and over SSH. |
| **llama.cpp integration** | **managed `llama-server` child process**, not FFI | See below. |
| Reqwest | adopted, **rustls with the `ring` provider** | Pure-Rust TLS. The default `aws-lc-rs` provider needs a C/CMake toolchain, which makes cross-compiling harder. |
| Markdown / syntax highlighting | **hand-written, small** | `syntect` adds megabytes of grammars and startup cost. A line-local highlighter covers keywords, strings, comments, numbers, calls and types well enough for chat. Easy to swap later. |
| SQLite (rusqlite, bundled) | deferred to the sessions milestone | Not needed for the first milestone. The model store uses an atomic JSON manifest (tiny, human-readable). |
| Credential storage | deferred | Keys come from environment variables today. Platform keychains (`keyring`) come with the providers/sessions milestone. |
| clap | adopted | Standard CLI parsing for headless commands. |

### Why a child process instead of linking llama.cpp

| | Child `llama-server` (chosen) | FFI (`llama-cpp-2` etc.) |
| --- | --- | --- |
| Crash isolation | a segfault in a GPU kernel can't take down the UI | the whole app dies |
| GPU backends | swap Metal/CUDA/Vulkan/ROCm builds without rebuilding 212 | each backend is a separate build of 212 |
| Build | 212 builds with plain `cargo`; no CMake or CUDA SDK | heavy native build, slow CI, hard cross-compilation |
| Updates | backend and app version independently | coupled |
| Features | streaming, Jinja chat templates, tool-call parsing, reasoning extraction come with the server | must be reimplemented |
| Cost | one extra process; loopback HTTP overhead (negligible next to token generation) | none |

The server is bound to `127.0.0.1` on a random free port with a random per-session API key, uses one slot (`--parallel 1`) so the whole context belongs to the conversation, and is supervised by 212:

- **Startup:** spawn → poll `/health` until ready (indeterminate progress, because llama.cpp doesn't report load progress) → detect early exit and turn the log tail into an actionable hint (OOM, unsupported architecture, corrupt file).
- **Shutdown:** SIGTERM, then SIGKILL after 3 s. Also `kill_on_drop` and a `Drop` impl.
- **Crash recovery:** each 212 process records its backend in `run/llama-server-<pid>.pid`. On startup, servers owned by dead 212 processes are killed. The process name is checked, so an unrelated process is never killed. On Linux, `PR_SET_PDEATHSIG` additionally kills the server if 212 dies.
- **Installation:** the official GitHub release asset for the platform is downloaded and its SHA-256 is checked against the digest GitHub publishes for that asset. It is then extracted with path-traversal checks and activated. Nothing runs before verification, and assets without a published digest are refused.

## Crates

```
crates/
  common     shared vocabulary: chat messages, stream events, UI⇄runtime protocol, paths
  config     TOML config: lenient load, validation, migration hook, comment-preserving edits
  models     GGUF header reader, memory estimates, model store, resumable downloader,
             Hugging Face client, Ollama registry + local-store reader, curated catalog
  inference  hardware detection, llama-server discovery, verified installer, process supervisor
  providers  Provider trait + OpenAI-compatible, Anthropic and mock implementations, SSE parser
  tools      typed agent tools: JSON-schema definitions, validation, workspace confinement, diffs, process control
  sessions   SQLite persistence (WAL, FTS5 search, schema versioning), Markdown/JSON export
  runtime    the orchestrator: conversation, agent loop, permission policy, sessions, compaction, skills,
             credentials (keychain), model lifecycle, downloads, providers, header stats/weather
  tui        Ratatui UI: themes, glyphs, ASCII art, animations, markdown, composer, overlays
  app        the `212` binary: CLI, logging, signals, headless mode
```

Dependency direction: `app → tui, runtime`; `runtime → providers, inference, models, config, common`; `tui → common, config`. The **TUI does not depend on the runtime, providers or inference**. It only speaks the protocol in `common::proto`.

## Data flow

```
 ┌──────────── tui (render loop) ────────────┐        ┌────────────── runtime (single owner) ─────────────┐
 │ crossterm events ─► App::on_key ─► Request ─┼──mpsc──► handle(Request)                                  │
 │                                            │        │   ├─ Send ─► spawn generation task ──┐            │
 │ App::on_core ◄─ Event ◄────────────────────┼──mpsc──┤   ├─ LoadModel ─► spawn load task ───┤ Internal   │
 │ view::draw (only when dirty / animating)   │        │   ├─ Pull ─► plan ─► Choice ─► spawn download ─┤  │
 └────────────────────────────────────────────┘        │   └─ … ◄────────── completion messages ◄──────┘  │
                                                        └───────────────────────────────────────────────────┘
                                                              │ HTTP (loopback, SSE)          │ child process
                                                              ▼                               ▼
                                                        Provider::chat ─────────────► llama-server (GGUF)
```

- **One owner for state.** The runtime task owns the conversation, the model store, the backend process and the active provider. It never awaits long operations. Those run in spawned tasks that stream `Event`s straight to the UI and report completion through an internal channel, so `Cancel` is always handled immediately.
- **Cancellation is dropping.** Stopping a generation drops the provider stream, which closes the HTTP connection, and llama.cpp stops decoding. Downloads and model loads use `CancellationToken`s. A partial reply is kept in history. A failed turn that produced nothing is removed so retrying doesn't duplicate it.
- **Approvals are protocol messages.** Anything costly or risky (multi-GB downloads, installing a backend) becomes an `Event::Choice` that the UI answers with `Request::Answer`. Headless mode asks on stdin or requires `--yes`. Tool permissions in the agent milestone will use the same mechanism.

## Rendering

- **Event-driven.** The loop redraws only on input, runtime events (bursts of tokens are coalesced into one frame) or an animation tick. The ≈12 FPS tick runs only while something visible animates (boot, loading, generating, downloading). Measured idle CPU is 0 ms over 10 s.
- **Incremental.** Each conversation entry caches its wrapped lines keyed by (width, theme generation, content version). While streaming, only the active reply is re-wrapped. Ratatui diffs the frame buffer, so the terminal receives only changed cells.
- **Themes are data.** Every color comes from semantic tokens (`bg`, `surface`, `text`, `muted`, `primary`, `secondary`, `border`, `selection_*`, `user`, `assistant`, `tool`, `success`, `warning`, `error`, `progress`, `code_bg`, `syn_*`). Colors are downgraded per terminal: truecolor → xterm-256 (nearest cube/gray) → 16 ANSI → monochrome, where emphasis falls back to bold, dim and reverse.
- **Glyph fallback.** All decorative glyphs go through `Glyphs`. In ASCII mode a final pass maps any remaining non-ASCII cell (including model output) to an ASCII stand-in.
- **Animations are pure functions of time**, so reduced motion means rendering frame 0 and never ticking.

## Model management

- **GGUF inspection** reads only the metadata section and skips large arrays (vocabularies) without materializing them. The skip goes through the read buffer, since a `seek` on `BufReader` would discard it.
- **Memory estimates** = weights (file size) + f16 KV cache (`layers × ctx × kv_heads × (k_dim + v_dim) × 2`, with context capped at the model maximum) + overhead. They are always labelled as estimates, and before download only a rough figure based on file size is shown.
- **Capabilities** (tool calling, reasoning, chat template, vision projector) are detected from the GGUF chat template and shown as declared, not guaranteed.
- **Store**: `models/<id>/<file>.gguf` + `models.json`. Imports are hard-linked when possible (no extra disk, and they survive deletion of the original). Otherwise the original path is referenced. Removing a model deletes only files 212 manages.
- **Downloads** write `<file>.part`, resume with HTTP `Range` (re-hashing existing bytes), verify size and SHA-256 (HF LFS oid, Ollama blob digest, GitHub asset digest), then rename into place. A mismatched file is deleted. Free disk space is checked before asking.
- **Ollama**: the registry is spoken directly (`/v2/<ns>/<name>/manifests/<tag>`, blobs by digest), and the daemon and CLI are never used. Adapter layers and missing model layers are rejected with an explanation. Templates and params are kept as metadata, while llama.cpp uses the Jinja template embedded in the GGUF. Models in a local `~/.ollama/models` store are imported in place.

## Security model (current)

- Local backend: loopback only, random port, random API key per session.
- Credentials: environment variables only, never logged, never serialized (provider `Debug` impls omit keys).
- `@file` attachments: resolved with `canonicalize` and must stay inside the working directory (symlink escapes are refused). Likely secrets are refused (`.env*`, private keys, `*.pem`/`*.key`, `.ssh`, `.aws`, `.gnupg`, `credentials`), binaries are refused, size is capped, and content is wrapped and labelled as data, not instructions.
- Downloads and backend binaries: checksum-verified before use. Archives are extracted with traversal protection.
- **Agent tools** (see the next section): every tool call is validated, confined to the workspace, approved according to the mode and policy, and recorded in `logs/audit.jsonl`.
- **Not implemented:** an OS-level sandbox for commands. Commands run with the user's privileges; the user's approval is the control.

## The agent

```
user message ─► model call (tools offered for the mode) ─► text? ─────────────► done
                     ▲                                  └► tool calls
                     │                                       │ tools::plan  (parse JSON args, resolve paths, compute diff / command)
                     │                                       │ policy       (mode × kind × session grants × risk) ─► ask user if needed
                     │                                       │ tools::execute (reads in parallel, the rest in order)
                     └──────────── tool results (data, capped 16k chars) ◄──┘
```

- **Tools** (`crates/tools`): `read_file`, `list_dir`, `find_files`, `grep`, `project_info`, `git_status`, `git_diff`, `git_log` (read); `write_file`, `edit_file` (write); `run_command`, `run_tests` (execute). Arguments are deserialized into typed structs, so wrong types or missing fields go back to the model as an error result, never into execution.
- **Plan, then execute.** Planning computes the exact effect. Edits keep the original content and refuse to apply if the file changed after the diff was shown. Writes are atomic (temp file + rename).
- **Workspace confinement.** Paths are normalized lexically (no `..` above the root, no absolute paths elsewhere) and then through `canonicalize`, so symlinks can't escape. `.git/` and credential-looking files are off limits.
- **Policy** (`runtime::permissions`): chat offers no tools; plan offers read-only tools and denies anything else even if the model calls it; build allows reads and asks for writes and commands. "Allow for session" grants are per kind for edits and per exact command line for commands, and are never offered for commands flagged risky. The risk heuristic is string-based and only adds warnings; approval remains the defense.
- **Processes.** `sh -c` (or `cmd /C`) runs in the workspace root with stdin closed, API-key variables removed, `TERM=dumb`/`NO_COLOR`/`CI=1`, a timeout (default 120 s, at most 600 s), and its own process group. That group is killed on timeout or `Esc`, including background children. Output is capped, keeping the head and tail.
- **Loop bounds.** `agent.max_steps` model calls per message. Connection failures before any output are retried twice with backoff. Older tool results in a long turn are shortened when the context fills (the user is told). A cancel at any point ends the turn, and every tool call still gets exactly one result message so the history stays valid.
- **Tool-call formats.** Native OpenAI-style tool calls (llama.cpp parses them from the model's chat template with `--jinja`). As a compatibility fallback, `<tool_call>{json}</tool_call>` blocks in plain text are parsed and validated like any other call. Local models whose template declares no tool support run without tools, and the user is told why.
- **Untrusted content.** The system prompt tells the model that tool results are data and must not be followed as instructions. Since no prompt is a guarantee, the permission prompts and confinement above are what actually protect the user.

## Major risks

| Risk | Mitigation |
| --- | --- |
| Small local models are unreliable at tool use | Use native tool calling via llama.cpp's Jinja templates. Show capability flags. Validate every tool call against its schema. Bound steps. Recommend tool-capable models. |
| llama.cpp release format/naming changes | Asset selection is pattern-based across the latest 10 releases. Users can always point `server_path` at any build. |
| Memory estimates are wrong for exotic architectures (MoE, sliding window) | Present them as estimates, err on the high side, and surface backend OOM errors with clear advice. |
| Terminal diversity (keyboard protocols, colors, glyphs) | Detect conservatively. Offer Alt+Enter and Ctrl+J as universal newline keys. Downgrade colors and glyphs. Config overrides exist. |
| Windows | Process supervision and paths are cross-platform. Untested so far (CI matrix configured). |
| Hugging Face / registry API changes or rate limits | Clear error messages. Downloads resume. The curated catalog doesn't depend on search. |

## Sessions and context

- **Storage.** One SQLite file (WAL mode, `PRAGMA user_version` migrations, FTS5 index kept in sync by triggers). Sessions are keyed by the canonical working directory. A session is created lazily on the first completed turn, and each turn (the user message plus every assistant/tool message) is appended when it finishes. Failed empty turns are not saved.
- **Compaction.** The current model summarizes everything except the last two exchanges, using a prompt that preserves instructions, decisions, paths and open tasks. The summary goes into the system prompt, and `compacted_upto` records which messages it covers. Nothing is deleted, so export and search still see the full transcript. Compaction runs automatically when context selection would otherwise drop messages, then sends the pending message, and the user is told either way.
- **Resume** restores the post-compaction history into context, shows the whole transcript, restores the mode, and notes when the session used a different model.

## Web, skills, credentials

- **`web_fetch`** (`tools::web`): http/https only, no embedded credentials. Hosts are refused if they are local names or resolve to loopback/private/link-local/CGNAT addresses (checked after DNS, and on every redirect, at most 5). Responses are capped at 3 MB and HTML is converted to text (scripts/styles/nav dropped, headings/lists/code kept). It is a `Network`-kind tool, so the policy asks per host with a session grant. `@https://` links in a user message are fetched without asking (the user typed them) and appended as labelled `<web_page>` data.
- **`web_search`** (`tools::web::search`): backends are DuckDuckGo's HTML page (parsed, ads skipped, redirect links unwrapped), a SearXNG instance (`format=json`) or the Brave Search API. The search grant is one key (`web:search`) for the session. The Brave key is read per turn from the environment or keychain and never printed (`SearchBackend`'s `Debug` hides it).
- **Skills** follow the open `SKILL.md` format. Only name and description are always in the prompt (progressive disclosure). `load_skill` returns the body plus a list of bundled files. Project skills override user skills. Installation copies folders (from git clones or local paths), skipping `.git` and symlinks, and executes nothing.
- **Credentials** come from the environment first, then the OS keychain (`keyring`: macOS Keychain, Windows Credential Manager, Secret Service). The TUI's key prompt is a separate masked overlay that bypasses input history.

## Extension points

- **Inference engines:** `inference::server` is the only place that knows about llama-server. Another engine can provide the same "spawn → ready → OpenAI-compatible URL" contract.
- **Providers:** implement `providers::Provider` (`chat` → event stream, `list_models`).
- **Tools:** add a `Spec` and an `Action` variant in `crates/tools`. The kind determines the default policy automatically. A documentation fetch tool (network, opt-in) and MCP-style external tools fit here.
- **Semantic indexing:** a future retrieval provider behind the same `@`-attachment and context-selection path. Deterministic search comes first.
