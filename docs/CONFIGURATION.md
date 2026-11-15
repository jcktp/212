# Configuration

212 reads one TOML file. `212 paths` prints its location. It is created with
comments on first run, and every key is optional: delete a line to get the
default back. Invalid values never stop 212 from starting. They are reset to
defaults and reported in the UI and in `/doctor`.

Settings changed from inside 212 (`/theme`, `/animations`, the active
provider, the last loaded model) are written back to the file **without
disturbing your comments or formatting**.

## Reference

### `[ui]`

| Key | Default | Values |
| --- | --- | --- |
| `theme` | `"phosphor"` | `phosphor`, `amber`, `dos`, `arcade`, `gameboy`, `cyber`, `paper`, `monochrome`, or a custom theme id |
| `animations` | `"on"` | `on`, `reduced` (static indicators, no ticking), `off` (also skips the boot sequence) |
| `animation_speed` | `1.0` | 0.25 – 4.0 |
| `ascii_art` | `true` | show logos and small illustrations |
| `startup_animation` | `true` | short, skippable boot sequence |
| `compact_mode` | `false` | trims header detail and the welcome logo |
| `glyphs` | `"auto"` | `auto`, `unicode`, `ascii` |
| `color_depth` | `"auto"` | `auto`, `truecolor`, `256`, `16`, `mono` (`NO_COLOR` forces `mono`) |
| `show_reasoning` | `true` | expand model reasoning by default (toggle with Ctrl+O) |

### `[inference]`

| Key | Default | |
| --- | --- | --- |
| `provider` | `"local"` | `local`, `ollama`, `openai`, `anthropic`, `compat` |
| `context_size` | `8192` | tokens. Capped at the model's maximum. Larger values need more memory. |
| `gpu_layers` | `"auto"` | `auto` lets llama.cpp decide, or a number of layers (`0` = CPU only) |
| `default_model` | unset | model id loaded at startup. Updated whenever you load a model. |
| `server_path` | unset | use this `llama-server` (e.g. a CUDA or Vulkan build) |
| `threads` | unset | CPU threads for llama.cpp |
| `temperature` | `0.7` | 0 – 2 |
| `top_p` | `0.95` | (0, 1] |
| `max_tokens` | unset | reply length limit |
| `load_timeout_secs` | `300` | how long to wait for a model to load |
| `system_prompt` | unset | replace the built-in system prompt |

### `[agent]`

| Key | Default | |
| --- | --- | --- |
| `default_mode` | `"chat"` | `chat`, `plan`, `build` |
| `max_steps` | `30` | 1 – 500. Agent loop bound (agent milestone). |
| `require_command_approval` | `true` | agent milestone |

### `[models]`

| Key | Default | |
| --- | --- | --- |
| `directory` | `"auto"` | or an absolute path for the model store |
| `huggingface_token_env` | `"HF_TOKEN"` | environment variable holding a Hugging Face token (gated models) |

### `[sessions]`

| Key | Default | |
| --- | --- | --- |
| `autosave` | `true` | sessions milestone |

### `[providers]`

| Key | Default |
| --- | --- |
| `ollama_url` | `"http://127.0.0.1:11434"` |
| `ollama_model` | unset (first available) |
| `openai_url` / `openai_model` / `openai_key_env` | `https://api.openai.com/v1` / `gpt-5` / `OPENAI_API_KEY` |
| `anthropic_url` / `anthropic_model` / `anthropic_key_env` | `https://api.anthropic.com` / `claude-sonnet-5-5` / `ANTHROPIC_API_KEY` |
| `compat_url` / `compat_model` / `compat_key_env` | unset. Any OpenAI-compatible API root, e.g. `http://localhost:1234/v1`. |

API keys are always read from the named environment variables. They are
never stored in this file.

## Environment variables

| Variable | |
| --- | --- |
| `TWO_ONE_TWO_HOME` | put config, data and cache under one directory |
| `TWO_ONE_TWO_LOG` | log filter, e.g. `debug` or `two_one_two_inference=trace` (logs go to the logs directory, never the terminal) |
| `NO_COLOR` | disable colors |
| `OLLAMA_MODELS` | location of a local Ollama model store to import from |
| `HF_TOKEN` | Hugging Face token for gated models (name configurable) |

## Custom themes

Put `<id>.toml` files in the `themes/` directory next to `config.toml`, then
use `/theme <id>`. A theme starts from a built-in `base` and overrides any
tokens you list:

```toml
name = "Midnight Teal"
description = "deep teal with warm accents"
base = "cyber"

[colors]
bg = "#081418"
text = "#d7f4f2"
primary = "#3fe0c5"
secondary = "#f2a65a"
```

Tokens: `bg`, `surface`, `text`, `muted`, `primary`, `secondary`, `border`,
`selection_bg`, `selection_fg`, `user`, `assistant`, `tool`, `success`,
`warning`, `error`, `progress`, `code_bg`, `syn_keyword`, `syn_string`,
`syn_comment`, `syn_number`, `syn_function`, `syn_type`.

Colors must be `#rrggbb`. Unknown tokens and malformed colors are reported in
the UI instead of being ignored. Built-in ids can't be overridden. Write
themes in truecolor; 212 downgrades them for 256/16-color and monochrome
terminals.

## Migrations

`config_version` records the schema version. Future versions migrate older
files in `Config::migrate`. A file from a newer 212 loads with a warning and
unknown keys are ignored.
