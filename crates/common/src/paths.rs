//! Platform-appropriate locations for configuration, data, cache and logs.
//!
//! Set `TWO_ONE_TWO_HOME` to place everything under one directory (used by
//! tests and portable installs).

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Self {
        if let Some(home) = std::env::var_os("TWO_ONE_TWO_HOME") {
            return Self::rooted(Path::new(&home));
        }
        match directories::ProjectDirs::from("", "", "212") {
            Some(d) => Self {
                config_dir: d.config_dir().to_path_buf(),
                data_dir: d.data_dir().to_path_buf(),
                cache_dir: d.cache_dir().to_path_buf(),
            },
            None => Self::rooted(&std::env::current_dir().unwrap_or_default().join(".212")),
        }
    }

    pub fn rooted(root: &Path) -> Self {
        Self { config_dir: root.join("config"), data_dir: root.join("data"), cache_dir: root.join("cache") }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }
    pub fn themes_dir(&self) -> PathBuf {
        self.config_dir.join("themes")
    }
    pub fn default_models_dir(&self) -> PathBuf {
        self.data_dir.join("models")
    }
    pub fn backends_dir(&self) -> PathBuf {
        self.data_dir.join("backends")
    }
    pub fn run_dir(&self) -> PathBuf {
        self.data_dir.join("run")
    }
    pub fn log_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }
    pub fn history_file(&self) -> PathBuf {
        self.data_dir.join("input_history")
    }
    pub fn sessions_db(&self) -> PathBuf {
        self.data_dir.join("sessions.sqlite3")
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        for d in [&self.config_dir, &self.data_dir, &self.cache_dir] {
            std::fs::create_dir_all(d)?;
        }
        std::fs::create_dir_all(self.log_dir())?;
        std::fs::create_dir_all(self.run_dir())?;
        Ok(())
    }
}
