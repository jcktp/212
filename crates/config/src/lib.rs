//! 212 configuration.
//!
//! The file is plain TOML. Unknown keys are ignored with a warning, invalid
//! values fall back to defaults with a warning, and targeted updates (e.g. the
//! theme picker) edit the document in place so user comments survive.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const CURRENT_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read {path}: {source}")]
    Read { path: PathBuf, source: std::io::Error },
    #[error("could not write {path}: {source}")]
    Write { path: PathBuf, source: std::io::Error },
    #[error("{path} is not valid TOML: {message}")]
    Parse { path: PathBuf, message: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub config_version: u32,
    pub ui: UiConfig,
    pub inference: InferenceConfig,
    pub agent: AgentConfig,
    pub models: ModelsConfig,
    pub sessions: SessionsConfig,
    pub providers: ProvidersConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            config_version: CURRENT_VERSION,
            ui: UiConfig::default(),
            inference: InferenceConfig::default(),
            agent: AgentConfig::default(),
            models: ModelsConfig::default(),
            sessions: SessionsConfig::default(),
            providers: ProvidersConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AnimationMode {
    #[default]
    On,
    Reduced,
    Off,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiConfig {
    pub theme: String,
    pub animations: AnimationMode,
    /// Multiplier for animation speed (0.25 – 4.0).
    pub animation_speed: f32,
    pub ascii_art: bool,
    pub startup_animation: bool,
    pub compact_mode: bool,
    /// `auto`, `unicode` or `ascii`.
    pub glyphs: String,
    /// `auto`, `truecolor`, `256`, `16` or `mono`.
    pub color_depth: String,
    pub show_reasoning: bool,
    /// Show CPU and memory use in the header.
    pub system_stats: bool,
    /// Place name for the header weather (Open-Meteo). Unset = off.
    pub weather: Option<String>,
    /// `celsius` or `fahrenheit`.
    pub weather_units: String,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "phosphor".into(),
            animations: AnimationMode::On,
            animation_speed: 1.0,
            ascii_art: true,
            startup_animation: true,
            compact_mode: false,
            glyphs: "auto".into(),
            color_depth: "auto".into(),
            show_reasoning: true,
            system_stats: true,
            weather: None,
            weather_units: "celsius".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InferenceConfig {
    /// `local`, `ollama`, `openai`, `anthropic` or `compat`.
    pub provider: String,
    pub context_size: u32,
    /// `auto` or a number of layers to offload to the GPU.
    pub gpu_layers: String,
    /// Model id from the local store that is loaded on startup.
    pub default_model: Option<String>,
    /// Override path to a `llama-server` executable.
    pub server_path: Option<PathBuf>,
    pub threads: Option<u32>,
    pub temperature: f32,
    pub top_p: f32,
    pub max_tokens: Option<u32>,
    /// Seconds to wait for the backend to load a model.
    pub load_timeout_secs: u64,
    pub system_prompt: Option<String>,
}

impl Default for InferenceConfig {
    fn default() -> Self {
        Self {
            provider: "local".into(),
            context_size: 8192,
            gpu_layers: "auto".into(),
            default_model: None,
            server_path: None,
            threads: None,
            temperature: 0.7,
            top_p: 0.95,
            max_tokens: None,
            load_timeout_secs: 300,
            system_prompt: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentConfig {
    pub default_mode: String,
    pub max_steps: u32,
    pub require_command_approval: bool,
    /// Web access for the model (`web_fetch`): `ask`, `allow` or `off`.
    pub web_access: String,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self { default_mode: "chat".into(), max_steps: 30, require_command_approval: true, web_access: "ask".into() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelsConfig {
    /// `auto` or an absolute path.
    pub directory: String,
    pub huggingface_token_env: String,
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self { directory: "auto".into(), huggingface_token_env: "HF_TOKEN".into() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SessionsConfig {
    pub autosave: bool,
    /// Summarize older turns automatically when the context window fills.
    pub auto_compact: bool,
}

impl Default for SessionsConfig {
    fn default() -> Self {
        Self { autosave: true, auto_compact: true }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProvidersConfig {
    pub ollama_url: String,
    pub ollama_model: Option<String>,
    pub openai_url: String,
    pub openai_model: String,
    pub openai_key_env: String,
    pub anthropic_url: String,
    pub anthropic_model: String,
    pub anthropic_key_env: String,
    /// Any OpenAI-compatible endpoint (LM Studio, vLLM, a remote llama-server...).
    pub compat_url: Option<String>,
    pub compat_model: Option<String>,
    pub compat_key_env: Option<String>,
}

impl Default for ProvidersConfig {
    fn default() -> Self {
        Self {
            ollama_url: "http://127.0.0.1:11434".into(),
            ollama_model: None,
            openai_url: "https://api.openai.com/v1".into(),
            openai_model: "gpt-5".into(),
            openai_key_env: "OPENAI_API_KEY".into(),
            anthropic_url: "https://api.anthropic.com".into(),
            anthropic_model: "claude-sonnet-5-5".into(),
            anthropic_key_env: "ANTHROPIC_API_KEY".into(),
            compat_url: None,
            compat_model: None,
            compat_key_env: None,
        }
    }
}

/// A loaded configuration plus anything worth telling the user about it.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub config: Config,
    pub warnings: Vec<String>,
    pub path: PathBuf,
    /// True when no file existed (first run).
    pub first_run: bool,
}

impl Config {
    /// Load from `path`. A missing file yields defaults with `first_run = true`.
    /// Invalid content never prevents startup: we warn and use defaults.
    pub fn load(path: &Path) -> Result<Loaded, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Loaded { config: Config::default(), warnings: vec![], path: path.into(), first_run: true });
            }
            Err(source) => return Err(ConfigError::Read { path: path.into(), source }),
        };
        let (config, warnings) = Self::parse_lenient(&text, path)?;
        Ok(Loaded { config, warnings, path: path.into(), first_run: false })
    }

    /// Parse strictly first; on failure fall back to a per-section lenient parse
    /// so that one bad key doesn't discard the rest of the user's settings.
    pub fn parse_lenient(text: &str, path: &Path) -> Result<(Config, Vec<String>), ConfigError> {
        let mut warnings = Vec::new();
        let mut config = match toml::from_str::<Config>(text) {
            Ok(c) => c,
            Err(strict_err) => {
                let table: toml::Table =
                    toml::from_str(text).map_err(|e| ConfigError::Parse { path: path.into(), message: e.message().to_string() })?;
                warnings.push(format!("config: {}", strict_err.message().trim()));
                let mut c = Config::default();
                macro_rules! section {
                    ($name:literal, $field:ident) => {
                        if let Some(v) = table.get($name) {
                            match v.clone().try_into() {
                                Ok(s) => c.$field = s,
                                Err(e) => warnings.push(format!(
                                    "config [{}]: {} — using defaults for this section",
                                    $name,
                                    e.to_string().trim()
                                )),
                            }
                        }
                    };
                }
                section!("ui", ui);
                section!("inference", inference);
                section!("agent", agent);
                section!("models", models);
                section!("sessions", sessions);
                section!("providers", providers);
                if let Some(v) = table.get("config_version").and_then(|v| v.as_integer()) {
                    c.config_version = v as u32;
                }
                c
            }
        };
        config.migrate(&mut warnings);
        config.validate(&mut warnings);
        Ok((config, warnings))
    }

    fn migrate(&mut self, warnings: &mut Vec<String>) {
        if self.config_version > CURRENT_VERSION {
            warnings.push(format!(
                "config_version {} is newer than this build understands ({CURRENT_VERSION}); some settings may be ignored",
                self.config_version
            ));
        }
        // Future migrations go here, keyed on `config_version`.
        self.config_version = self.config_version.max(CURRENT_VERSION);
    }

    /// Clamp or reset out-of-range values, recording a warning for each.
    pub fn validate(&mut self, warnings: &mut Vec<String>) {
        let d = Config::default();
        if !(0.25..=4.0).contains(&self.ui.animation_speed) {
            warnings.push(format!("ui.animation_speed {} out of range 0.25–4.0; using 1.0", self.ui.animation_speed));
            self.ui.animation_speed = 1.0;
        }
        if !["auto", "unicode", "ascii"].contains(&self.ui.glyphs.as_str()) {
            warnings.push(format!("ui.glyphs '{}' invalid; using auto", self.ui.glyphs));
            self.ui.glyphs = d.ui.glyphs.clone();
        }
        if !["auto", "truecolor", "256", "16", "mono"].contains(&self.ui.color_depth.as_str()) {
            warnings.push(format!("ui.color_depth '{}' invalid; using auto", self.ui.color_depth));
            self.ui.color_depth = d.ui.color_depth.clone();
        }
        if !(512..=1_048_576).contains(&self.inference.context_size) {
            warnings.push(format!("inference.context_size {} out of range; using 8192", self.inference.context_size));
            self.inference.context_size = d.inference.context_size;
        }
        if self.inference.gpu_layers != "auto" && self.inference.gpu_layers.parse::<u32>().is_err() {
            warnings.push(format!("inference.gpu_layers '{}' must be 'auto' or a number", self.inference.gpu_layers));
            self.inference.gpu_layers = d.inference.gpu_layers.clone();
        }
        if !(0.0..=2.0).contains(&self.inference.temperature) {
            warnings.push("inference.temperature must be within 0–2; using 0.7".into());
            self.inference.temperature = d.inference.temperature;
        }
        if !(0.0..=1.0).contains(&self.inference.top_p) || self.inference.top_p == 0.0 {
            warnings.push("inference.top_p must be within (0, 1]; using 0.95".into());
            self.inference.top_p = d.inference.top_p;
        }
        if !["local", "ollama", "openai", "anthropic", "compat"].contains(&self.inference.provider.as_str()) {
            warnings.push(format!("inference.provider '{}' unknown; using local", self.inference.provider));
            self.inference.provider = d.inference.provider.clone();
        }
        if !["chat", "plan", "build"].contains(&self.agent.default_mode.as_str()) {
            warnings.push(format!("agent.default_mode '{}' unknown; using chat", self.agent.default_mode));
            self.agent.default_mode = d.agent.default_mode.clone();
        }
        if !["ask", "allow", "off"].contains(&self.agent.web_access.as_str()) {
            warnings.push(format!("agent.web_access '{}' must be ask, allow or off; using ask", self.agent.web_access));
            self.agent.web_access = d.agent.web_access.clone();
        }
        if !["celsius", "fahrenheit"].contains(&self.ui.weather_units.as_str()) {
            warnings.push("ui.weather_units must be celsius or fahrenheit; using celsius".into());
            self.ui.weather_units = d.ui.weather_units.clone();
        }
        if self.agent.max_steps == 0 || self.agent.max_steps > 500 {
            warnings.push("agent.max_steps must be within 1–500; using 30".into());
            self.agent.max_steps = d.agent.max_steps;
        }
        if self.models.directory != "auto" && !Path::new(&self.models.directory).is_absolute() {
            warnings.push("models.directory must be 'auto' or an absolute path; using auto".into());
            self.models.directory = d.models.directory.clone();
        }
    }

    /// Write a fresh, commented configuration file. Used on first run.
    pub fn write_default(path: &Path) -> Result<(), ConfigError> {
        atomic_write(path, DEFAULT_TEMPLATE)
    }
}

/// Set one `section.key` value in the file at `path`, preserving comments
/// and formatting elsewhere. Creates the file if needed.
pub fn set_value(path: &Path, section: &str, key: &str, value: toml_edit::Value) -> Result<(), ConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DEFAULT_TEMPLATE.to_string(),
        Err(source) => return Err(ConfigError::Read { path: path.into(), source }),
    };
    let mut doc: toml_edit::DocumentMut =
        text.parse().map_err(|e: toml_edit::TomlError| ConfigError::Parse { path: path.into(), message: e.to_string() })?;
    if !doc.contains_table(section) {
        doc[section] = toml_edit::table();
    }
    doc[section][key] = toml_edit::Item::Value(value);
    atomic_write(path, &doc.to_string())
}

fn atomic_write(path: &Path, contents: &str) -> Result<(), ConfigError> {
    let err = |source| ConfigError::Write { path: path.into(), source };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(err)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, contents).map_err(err)?;
    std::fs::rename(&tmp, path).map_err(err)
}

pub const DEFAULT_TEMPLATE: &str = r#"# 212 configuration
# Every key is optional; delete a line to get the default back.
config_version = 1

[ui]
theme = "phosphor"          # phosphor, amber, dos, arcade, gameboy, cyber, paper, monochrome, or a custom theme
animations = "on"           # on, reduced, off
animation_speed = 1.0       # 0.25 – 4.0
ascii_art = true
startup_animation = true
compact_mode = false
glyphs = "auto"             # auto, unicode, ascii
color_depth = "auto"        # auto, truecolor, 256, 16, mono
show_reasoning = true
system_stats = true         # CPU / memory in the header
# weather = "Amsterdam"     # header weather via open-meteo.com (sends only this place name)
weather_units = "celsius"   # celsius, fahrenheit

[inference]
provider = "local"          # local, ollama, openai, anthropic, compat
context_size = 8192
gpu_layers = "auto"         # auto, or a number of layers
temperature = 0.7
top_p = 0.95
load_timeout_secs = 300
# default_model = "qwen3-0.6b-q8_0"
# server_path = "/path/to/llama-server"

[agent]
default_mode = "chat"       # chat, plan, build
max_steps = 30
require_command_approval = true
web_access = "ask"          # web_fetch tool and @https:// links: ask, allow, off

[models]
directory = "auto"
huggingface_token_env = "HF_TOKEN"

[sessions]
autosave = true
auto_compact = true         # summarize older turns when the context fills

[providers]
ollama_url = "http://127.0.0.1:11434"
openai_url = "https://api.openai.com/v1"
openai_model = "gpt-5"
openai_key_env = "OPENAI_API_KEY"
anthropic_url = "https://api.anthropic.com"
anthropic_model = "claude-sonnet-5-5"
anthropic_key_env = "ANTHROPIC_API_KEY"
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_template_parses_cleanly() {
        let (c, w) = Config::parse_lenient(DEFAULT_TEMPLATE, Path::new("t")).unwrap();
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c, Config::default());
    }

    #[test]
    fn missing_file_is_first_run() {
        let dir = tempfile::tempdir().unwrap();
        let l = Config::load(&dir.path().join("none.toml")).unwrap();
        assert!(l.first_run);
        assert_eq!(l.config, Config::default());
    }

    #[test]
    fn invalid_values_are_clamped_with_warnings() {
        let text = "[ui]\nanimation_speed = 99.0\nglyphs = \"wingdings\"\n[inference]\ngpu_layers = \"lots\"\n";
        let (c, w) = Config::parse_lenient(text, Path::new("t")).unwrap();
        assert_eq!(c.ui.animation_speed, 1.0);
        assert_eq!(c.ui.glyphs, "auto");
        assert_eq!(c.inference.gpu_layers, "auto");
        assert_eq!(w.len(), 3);
    }

    #[test]
    fn unknown_key_keeps_other_sections() {
        let text = "[ui]\ntheme = \"amber\"\nbogus = 1\n[inference]\ncontext_size = 4096\n";
        let (c, w) = Config::parse_lenient(text, Path::new("t")).unwrap();
        // The [ui] section is rejected as a whole, but [inference] survives.
        assert_eq!(c.inference.context_size, 4096);
        assert!(!w.is_empty());
    }

    #[test]
    fn broken_toml_is_an_error() {
        assert!(Config::parse_lenient("[ui\n", Path::new("t")).is_err());
    }

    #[test]
    fn set_value_preserves_comments() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        Config::write_default(&p).unwrap();
        set_value(&p, "ui", "theme", "amber".into()).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("# 212 configuration"));
        let l = Config::load(&p).unwrap();
        assert_eq!(l.config.ui.theme, "amber");
        assert!(l.warnings.is_empty());
    }

    #[test]
    fn set_value_creates_missing_section() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(&p, "config_version = 1\n").unwrap();
        set_value(&p, "inference", "default_model", "abc".into()).unwrap();
        let l = Config::load(&p).unwrap();
        assert_eq!(l.config.inference.default_model.as_deref(), Some("abc"));
    }
}
