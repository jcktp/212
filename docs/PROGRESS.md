# Progress

_Last updated: 2026-11-15 · version 0.1.0 · Phase 6 complete_

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

## Fix: local files and web in chat mode

- **Problem:** in the default chat mode the model only had `web_fetch`, so asked about the project it tried `web_fetch file:///…`, failed, and then guessed. Models without tool support were told nothing and could pretend to have read things.
- **Chat mode** now offers the read-only file tools (as in plan mode). Edits and commands still need build mode.
- **`web_search`** tool with DuckDuckGo (default, no key), SearXNG or Brave backends. A bot-check refusal is reported as such, with a hint to `web_fetch` a known URL.
- **Honesty:** the prompt forbids claiming tool use that didn't happen; models without tool support get a prompt saying they have no tools and pointing at `@file`/`@url`. Empty (JavaScript-only) pages are returned as failures, not as empty successes.
- **Inline thinking** (`<|channel>thought…<channel|>`, `<think>…</think>`) that a server streams as content is moved to reasoning.
- **Fixed:** a quick exit (`212 -p`) could lose the last turn from the session database.
- Verified with Gemma 4 12B: asked for a project breakdown, it called `project_info`, `list_dir` and `read_file` and answered from the real files. With search blocked it said so instead of inventing results.

## Phase 6 (persistence and providers) and requested extras: done

Verified in a real terminal (tmux) with Gemma 4 12B on the managed backend:

- **Model switching from one list.** In a fresh home with only `llava-phi3` installed, `/models` listed llava `[loaded]`, `gemma4:12b [on disk]` and six `[download]` recommendations. One Enter on Gemma imported it from the Ollama store (hard link, no download), loaded it, and stopped the llava server (one llama-server left). Removal asks for confirmation and says what is deleted versus kept.
- **Sessions.** After quitting, `212 -c` restored the transcript and mode and autoloaded the model. `/history example domain` found the earlier session by full text (with a highlighted snippet). `/export` wrote Markdown with session metadata.
- **Links and web.** `@https://example.com` was fetched and answered from. A plain README link made the model call `web_fetch`; the approval offered "Allow raw.githubusercontent.com this session", and the answer used the fetched README.
- **Skills.** `/skills add <folder>` installed a sample `haiku-writer` skill. On "Write a haiku about terminals" the model called `load_skill` itself and followed the skill's format exactly.
- **Header.** `cpu 12% · ram 13.1G/16.0G · model 7.7G │ ☁ 9°C Amsterdam` (weather via `/weather Amsterdam`).

Bugs found and fixed while verifying: session metadata recorded an empty model name when a model loaded (fixed ordering); a lock-ordering deadlock in session lookup by id prefix (caught by tests); Windows-only lint failures in CI.

## Phase 5 (agent runtime): done

The acceptance criterion ("inspect a repository, propose changes, modify
approved files, run tests, and report results") was run for real with
**Gemma 4 12B (Q4_K_M)**, imported by hard link from the local Ollama store,
served by the managed llama.cpp backend on Metal at about 11 tok/s, in a real
terminal. Demo project: a two-function Python module with a bug
(`add` subtracts) and two failing unittest tests.

1. `212 --mode build`, prompt: *"The tests are failing. Find the bug, fix it, and run the tests to confirm."*
2. The agent asked to run `make test` (approval dialog showing the exact command), saw both tests fail, listed the project and read `calc.py`.
3. It sent `edit_file` twice with a snippet that didn't match the file exactly (it over-escaped the docstring quotes). Both were refused with "old_string was not found"; it re-read the file and proposed the right edit.
4. The approval dialog showed the diff (`-    return a - b` / `+    return a + b`); approved once, applied atomically.
5. It re-ran `make test` (approved with *Always allow this command this session*): `Ran 2 tests … OK`. It then summarized the fix.
6. A follow-up "run the tests once more" executed without a prompt (session grant). All 9 tool calls are in `logs/audit.jsonl`. Exiting left no llama-server running.

## Phase status

| Phase | Status |
| --- | --- |
| 1 Foundation | ✅ workspace, config (validation, comment-preserving writes), logging to file, tests |
| 2 Terminal interface | ✅ |
| 3 Local inference | ✅ |
| 4 Model management | ✅ except `/models benchmark` and vision projectors |
| 5 Agent runtime | ✅ tools, permission policy, agent loop, chat/plan/build modes, diff previews, audit log |
| 6 Persistence and providers | ✅ SQLite sessions (save, resume, rename, delete, per project), full-text history, Markdown/JSON export, manual and automatic compaction, keychain credentials, `-c`/`--resume` |
| 7 Polish and release | 🟡 onboarding and benchmarks done; packaging and cross-platform testing not started |

## What works

- **TUI:** header (model, provider, cwd, live status), conversation with Markdown (headings, lists, quotes, rules, inline code/bold/italic/links), syntax-highlighted code blocks, collapsible reasoning, per-reply stats (tokens, tok/s, time), scrollback with scrollbar, multiline composer with persistent history, `/` and `@file` completion, OSC 52 copy, context meter. Rendering is event-driven, and terminal resize is handled.
- **Themes:** 8 built-in themes plus validated custom TOML themes, switchable at runtime. Truecolor → 256 → 16 → mono degradation and `NO_COLOR`.
- **Animations:** six styles (spinner, bounce, blocks, bar, scan, pulse) at ≈12 FPS only while visible. `on`/`reduced`/`off` and a speed setting are supported, and the boot sequence is skippable.
- **Local inference:** discovery (config → managed → PATH), verified install from official releases, loopback-only server with a random port and key, health checks, load-error diagnosis, model switching, `/unload`, cancellation, orphan reaping.
- **Models:** GGUF metadata reader, memory estimates with fit verdicts, capability detection, a hard-link-based store, resumable SHA-256-verified downloads with progress/speed/ETA, HF search and pull (quant selection, split files, license, gated hint, disk-space check), Ollama registry pull without the daemon, local Ollama store import, curated recommendations by hardware.
- **Providers:** local, Ollama, OpenAI, Anthropic and OpenAI-compatible, switchable at runtime. Streaming, usage, measured speed and clear auth/connection errors.
- **Headless:** `212 -p`, piped stdin, `212 models …`, `212 doctor`, `212 backend`, `212 paths`. Reasoning goes to stderr so stdout stays pipe-clean. Exit codes are meaningful.
- **Agent:** 12 typed tools (read/list/find/grep/project info/git status·diff·log/write/edit/run command/run tests), JSON-schema definitions, argument validation, workspace confinement (lexical + symlink), protected `.git/` and credential files, plan→approve→execute with diffs computed before anything changes, stale-edit detection, atomic writes, process-group kill on timeout/cancel, API keys scrubbed from command environments, allow once / for session / deny, risky-command detection, parallel reads, bounded steps and retries, in-turn context shortening, text tool-call fallback, an audit log, and mode enforcement even when the model calls a tool that wasn't offered. The UI shows a live tool activity line ("reading / searching / editing / running tests / waiting for your approval"), tool results with diffs and output tails, and expands everything with Ctrl+O.
- **Safety:** `@file` attachments are confined to the workspace (no symlink escapes), secret-looking files are refused, size is capped, and content is labelled as data. Explicit approval is required for every download and backend install.

## Known limitations

- **No OS sandbox for commands.** They run with your user's privileges in the project directory. The approval dialog is the safeguard, and the risky-command detector is a heuristic warning, not a guarantee.
- **Small models are weak agents.** Qwen3-0.6B declares tool support but rarely uses tools well. Gemma 4 12B worked, but needed retries on exact-match edits. Prefer 8B+ models for build mode, or a cloud provider.
- **No undo/checkpoint** for applied edits (use git).
- **The context meter** updates after each message, not live during a multi-step turn.
- **Keychain storage** is implemented and compiles on all platforms, but writing to the real macOS Keychain was not exercised during testing (to avoid modifying it). Environment variables work as before.
- **Compaction quality** depends on the model; small models write weaker summaries. The full transcript is always kept.
- **Web:** no JavaScript rendering (such pages are reported to the model as unreadable) and no PDFs. The default DuckDuckGo search may be refused by its bot check after a burst of queries; SearXNG or Brave (`agent.search`) are the reliable options.
- **Skills:** bundled scripts run through `run_command` (build mode, with approval). Models without tool support can only use skills activated with `/skill`.
- **Model loading progress** is indeterminate, because llama.cpp exposes no load progress.
- **Downloads** can be cancelled and resumed (`Esc`, then pull again). There is no separate pause button.
- **Backend installer** picks the portable CPU build on Linux/Windows. Use `inference.server_path` for CUDA/Vulkan builds.
- **Untested platforms:** Linux and Windows are expected to work (CI is configured for all three) but have not been run yet. Tested terminal: tmux 3.8. Others on the list (Terminal.app, iTerm2, Ghostty, Kitty, WezTerm, Alacritty, Windows Terminal) still need a manual pass.
- **Not tested live:** the OpenAI and Anthropic success paths (no API keys were available; the OpenAI 401 path was verified), and gated Hugging Face downloads.
- **Distribution:** source build only. No Homebrew formula or release binaries yet.

## Test results

`cargo fmt --check` ✅ · `cargo clippy --all-targets -- -D warnings` ✅ · `cargo test`: **161 passed, 0 failed, 1 ignored**

| Suite | Tests | Covers |
| --- | --- | --- |
| common | 2 | formatting |
| config | 7 | defaults, lenient parsing, validation, comment-preserving writes |
| models | 24 | GGUF parsing (incl. truncated/corrupt), estimates, capabilities, store import/remove/hard links, downloads (verify, **resume after interruption**, server ignoring Range, checksum mismatch, cancel), HF spec/quant/split selection, Ollama refs/manifests/local store, catalog |
| inference | 9 | backend discovery and versions, **process lifecycle** (early exit diagnosis, cancel + shutdown kills the process), orphan reaping never kills unrelated processes, release asset selection, archive traversal protection |
| providers | 8 | SSE parsing across chunk boundaries/UTF-8, OpenAI chunk mapping (text/reasoning/tools/usage/speed/errors), real HTTP streaming, connect errors, key never leaks, Anthropic body/events |
| tools | 14 | argument validation, path confinement and symlink escapes, protected files, read/list/find/grep with .gitignore, write/edit diffs, stale-edit refusal, exit codes, env scrubbing, **timeout and cancel kill the whole process group**, git status, project and test-command detection |
| runtime | 9 + 9 + 10 | context selection, `@file` safety, policy matrix, audit log, text tool-call fallback; **mock-provider integration**: streaming, history, cancellation keeps partial text, failed turns not duplicated, retries, concurrency guard, shutdown, errors; **agent loop**: reads run unasked and feed back, approved edit applies the diff, denied edit leaves the file and informs the model, session grant, plan mode refuses writes, chat offers no tools, invalid args become tool errors, step limit, cancel kills a running command, cancel dismisses a pending approval |
| sessions | 5 | round trip, listing, rename/delete cascade, FTS search incl. hostile query syntax, compaction metadata, Markdown/JSON export, on-disk persistence, refusal of newer schemas |
| runtime (sessions) | 5 | persist → restart → `--continue` → model sees history; list/search/rename/export; separate sessions after `/new` and confirmed delete; manual compaction keeps the last two exchanges; automatic compaction when the window fills; model removal asks first |
| tui | 28 + 18 | themes (all 8, custom, validation, color degradation), glyphs, art, animations (static under reduced motion), markdown wrapping/highlighting, composer, commands; **TestBackend UI**: every theme at 5 sizes, picker preview/restore/persist, sending, streaming render, slash → request mapping, Ctrl+C semantics, dialogs, ASCII-only output, monochrome output, idle never ticks, completion, tool activity and diff rendering, permission dialog preview/answer/dismiss, template-token stripping |
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

1. **Agent polish:** checkpoints/undo for applied edits, a live context meter during turns, a pluggable web search provider, and optional OS sandboxing (macOS `sandbox-exec` profiles, Linux namespaces/landlock).
2. **Release (Phase 7):** CI on all three OSes, release binaries with checksums, a Homebrew tap, a terminal compatibility pass, `/models benchmark`, vision projector support.
