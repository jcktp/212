# Progress

_Last updated: 2026-11-15 · version 0.1.0_

## First working milestone: done

Each step was run on macOS 27 (Apple Silicon, 16 GB) in a real terminal
(tmux-driven pty) and headlessly. Ollama was **not** used: its desktop app
happened to be running on the test machine, but 212 never contacts the
daemon. The managed-backend run also had Homebrew removed from `PATH`.

| # | Requirement | Evidence |
| --- | --- | --- |
| 1 | Launch `212` in a terminal | release binary, first frame ≈30 ms |
| 2 | Compact ASCII identity | boot logo, compact welcome logo, `◢212◣` header, ASCII fallbacks |
| 3 | Select one of eight themes | `/theme` and `Ctrl+T` picker with live preview, Esc restores, persisted to config |
| 4 | Keyboard navigation | composer, history, completion, pickers, dialogs, scrolling; no mouse needed |
| 5 | Select/import a GGUF model | `/models`, `/models import`, local Ollama store import, HF and Ollama-registry pulls |
| 6 | Managed local backend | spawned, health-checked, switched and stopped by 212. Verified installer tested from GitHub. |
| 7–8 | Type a message and receive a real streamed reply | Qwen3-0.6B Q8_0: 108–120 tok/s; llava-phi3 (Q4_K_M from the Ollama store): ~38 tok/s |
| 9 | Status animations | boot sequence, loading bar, spinner, pulse, bounce, scan (checksum), download bar |
| 10 | Clean exit with no background processes | Ctrl+C, `/quit`, SIGTERM, SIGHUP all stop llama-server. After SIGKILL the next launch reaps the orphan. |

## Phase status

| Phase | Status |
| --- | --- |
| 1 Foundation | ✅ workspace, config (validation, comment-preserving writes), logging to file, tests |
| 2 Terminal interface | ✅ |
| 3 Local inference | ✅ |
| 4 Model management | ✅ except `/models benchmark` and vision projectors |
| 5 Agent runtime | ⏳ not started. Modes exist and are displayed, but the model has no tools yet. |
| 6 Persistence and providers | 🟡 providers done; sessions/SQLite, export, history search, keychain storage and compaction not started |
| 7 Polish and release | 🟡 onboarding and benchmarks done; packaging and cross-platform testing not started |

## What works

- **TUI:** header (model, provider, cwd, live status), conversation with Markdown (headings, lists, quotes, rules, inline code/bold/italic/links), syntax-highlighted code blocks, collapsible reasoning, per-reply stats (tokens, tok/s, time), scrollback with scrollbar, multiline composer with persistent history, `/` and `@file` completion, OSC 52 copy, context meter. Rendering is event-driven, and terminal resize is handled.
- **Themes:** 8 built-in themes plus validated custom TOML themes, switchable at runtime. Truecolor → 256 → 16 → mono degradation and `NO_COLOR`.
- **Animations:** six styles (spinner, bounce, blocks, bar, scan, pulse) at ≈12 FPS only while visible. `on`/`reduced`/`off` and a speed setting are supported, and the boot sequence is skippable.
- **Local inference:** discovery (config → managed → PATH), verified install from official releases, loopback-only server with a random port and key, health checks, load-error diagnosis, model switching, `/unload`, cancellation, orphan reaping.
- **Models:** GGUF metadata reader, memory estimates with fit verdicts, capability detection, a hard-link-based store, resumable SHA-256-verified downloads with progress/speed/ETA, HF search and pull (quant selection, split files, license, gated hint, disk-space check), Ollama registry pull without the daemon, local Ollama store import, curated recommendations by hardware.
- **Providers:** local, Ollama, OpenAI, Anthropic and OpenAI-compatible, switchable at runtime. Streaming, usage, measured speed and clear auth/connection errors.
- **Headless:** `212 -p`, piped stdin, `212 models …`, `212 doctor`, `212 backend`, `212 paths`. Reasoning goes to stderr so stdout stays pipe-clean. Exit codes are meaningful.
- **Safety:** `@file` attachments are confined to the workspace (no symlink escapes), secret-looking files are refused, size is capped, and content is labelled as data. Explicit approval is required for every download and backend install.

## Known limitations

- **No agent tools yet.** `plan`/`build` modes change only the label. File reading/editing, shell execution, the permission system and diff previews come in the next milestone.
- **No persistent sessions.** The conversation lives in memory. `/new`, `/clear` and `/context` work; `/sessions`, `/resume`, `/history` and `/export` are not implemented.
- **Context:** when the window fills, the oldest turns are left out and the user is told. There is no summarizing compaction yet (`/compact` explains this).
- **Credentials** come from environment variables only (no keychain integration yet).
- **Model loading progress** is indeterminate, because llama.cpp exposes no load progress.
- **Downloads** can be cancelled and resumed (`Esc`, then pull again). There is no separate pause button.
- **Backend installer** picks the portable CPU build on Linux/Windows. Use `inference.server_path` for CUDA/Vulkan builds.
- **Untested platforms:** Linux and Windows are expected to work (CI is configured for all three) but have not been run yet. Tested terminal: tmux 3.8. Others on the list (Terminal.app, iTerm2, Ghostty, Kitty, WezTerm, Alacritty, Windows Terminal) still need a manual pass.
- **Not tested live:** the OpenAI and Anthropic success paths (no API keys were available; the OpenAI 401 path was verified), and gated Hugging Face downloads.
- **Distribution:** source build only. No Homebrew formula or release binaries yet.

## Test results

`cargo fmt --check` ✅ · `cargo clippy --all-targets -- -D warnings` ✅ · `cargo test`: **107 passed, 0 failed, 1 ignored**

| Suite | Tests | Covers |
| --- | --- | --- |
| common | 2 | formatting |
| config | 7 | defaults, lenient parsing, validation, comment-preserving writes |
| models | 24 | GGUF parsing (incl. truncated/corrupt), estimates, capabilities, store import/remove/hard links, downloads (verify, **resume after interruption**, server ignoring Range, checksum mismatch, cancel), HF spec/quant/split selection, Ollama refs/manifests/local store, catalog |
| inference | 9 | backend discovery and versions, **process lifecycle** (early exit diagnosis, cancel + shutdown kills the process), orphan reaping never kills unrelated processes, release asset selection, archive traversal protection |
| providers | 8 | SSE parsing across chunk boundaries/UTF-8, OpenAI chunk mapping (text/reasoning/tools/usage/speed/errors), real HTTP streaming, connect errors, key never leaks, Anthropic body/events |
| runtime | 6 + 8 | context selection, `@file` safety; **mock-provider integration**: streaming, history, cancellation keeps partial text, failed turns not duplicated, concurrency guard, new conversation, shutdown, error reporting |
| tui | 27 + 12 | themes (all 8, custom, validation, color degradation), glyphs, art, animations (static under reduced motion), markdown wrapping/highlighting, composer, commands; **TestBackend UI**: every theme at 5 sizes, picker preview/restore/persist, sending, streaming render, slash → request mapping, Ctrl+C semantics, dialogs, ASCII-only output, monochrome output, idle never ticks, completion |
| app | 4 + 1 ignored | CLI smoke tests; `real_inference` (opt-in, passes: import → load → stream → clean exit) |

Run the real-inference test:

```bash
TWO_ONE_TWO_TEST_GGUF=/path/to/Qwen3-0.6B-Q8_0.gguf cargo test -p two-one-two -- --ignored
```

## Benchmarks (release build, M-series Mac, 16 GB)

| Metric | Target | Measured |
| --- | --- | --- |
| `212 --version` | — | 4.3 ms median (20 runs) |
| Time to first interactive frame | < 200 ms | ≈30 ms (5 runs, includes tmux spawn and polling) |
| Idle CPU (model loaded, UI idle) | ≈0 | 0 ms CPU over 10 s |
| UI process memory | < 50 MB | 15.5 MB RSS (15.8 MB after a reply) |
| Backend memory (separate) | — | 3.8 GB RSS for llava-phi3 Q4_K_M at 4k ctx |
| Release binary | — | 7.5 MB |
| Qwen3-0.6B Q8_0 cold start → first answer (headless) | — | 1.5 s total |

## Next milestones

1. **Agent runtime (Phase 5):** `tools` crate (read/list/search/glob/edit/write/shell/git/tests) with JSON-schema validation and structured results. Workspace-confined paths, a permission policy (allow once / for session / deny) through the existing `Choice` protocol, diff previews, and an audit log. A bounded agent loop in the runtime with timeouts and retries. Mode enforcement (chat: no tools; plan: read-only; build: edits and approved commands). Native tool calling via llama.cpp `--jinja`, with a clearly labelled fallback parser.
2. **Sessions (Phase 6):** SQLite (rusqlite, bundled): sessions per project, resume, rename, delete, search, export to Markdown/JSON, provider/model metadata. Summarizing `/compact`. Keychain credential storage.
3. **Release (Phase 7):** CI on all three OSes, release binaries with checksums, a Homebrew tap, a terminal compatibility pass, `/models benchmark`, vision projector support.
