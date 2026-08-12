use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

const CONFIG_SCHEMA: u32 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub schema: u32,
    pub model_path: Option<PathBuf>,
    pub llama_server_path: Option<PathBuf>,
    pub server_url: Option<String>,
    pub threads: usize,
    pub context_size: usize,
    pub max_tokens: usize,
    pub startup_timeout_seconds: u64,
    pub show_tab_hint: bool,
    pub shell_path: PathBuf,
    pub model_id: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema: CONFIG_SCHEMA,
            model_path: None,
            llama_server_path: None,
            server_url: None,
            threads: std::thread::available_parallelism()
                .map_or(4, std::num::NonZero::get)
                .clamp(1, 8),
            context_size: 2_048,
            max_tokens: 96,
            startup_timeout_seconds: 90,
            show_tab_hint: true,
            shell_path: PathBuf::from(if cfg!(target_os = "macos") {
                "/bin/zsh"
            } else {
                "/bin/bash"
            }),
            model_id: "howto".into(),
        }
    }
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        if self.schema > CONFIG_SCHEMA {
            return Err(Error::Configuration(format!(
                "config schema {} is newer than this HowTo supports ({CONFIG_SCHEMA})",
                self.schema
            )));
        }
        if self.schema < CONFIG_SCHEMA {
            return Err(Error::Configuration(format!(
                "config schema {} is older than the supported schema ({CONFIG_SCHEMA})",
                self.schema
            )));
        }
        if !(1..=512).contains(&self.threads) {
            return Err(Error::Configuration(
                "threads must be between 1 and 512".into(),
            ));
        }
        if !(256..=131_072).contains(&self.context_size) {
            return Err(Error::Configuration(
                "context_size must be between 256 and 131072".into(),
            ));
        }
        if !(16..=4_096).contains(&self.max_tokens) {
            return Err(Error::Configuration(
                "max_tokens must be between 16 and 4096".into(),
            ));
        }
        if self.max_tokens >= self.context_size {
            return Err(Error::Configuration(
                "max_tokens must be smaller than context_size".into(),
            ));
        }
        if self.startup_timeout_seconds == 0 || self.startup_timeout_seconds > 600 {
            return Err(Error::Configuration(
                "startup_timeout_seconds must be between 1 and 600".into(),
            ));
        }
        if self.model_id.trim().is_empty() {
            return Err(Error::Configuration("model_id may not be empty".into()));
        }
        if self.model_id.len() > 128
            || !self.model_id.chars().all(|character| {
                character.is_ascii_alphanumeric()
                    || matches!(character, '.' | '_' | '-' | '/' | ':')
            })
        {
            return Err(Error::Configuration(
                "model_id must be at most 128 ASCII letters, digits, or . _ - / : characters"
                    .into(),
            ));
        }
        if !self.shell_path.is_absolute() {
            return Err(Error::Configuration(
                "shell_path must be an absolute path".into(),
            ));
        }
        for (key, path) in [
            ("model_path", self.model_path.as_deref()),
            ("llama_server_path", self.llama_server_path.as_deref()),
        ] {
            if path.is_some_and(|path| !path.is_absolute()) {
                return Err(Error::Configuration(format!(
                    "{key} must be an absolute path"
                )));
            }
        }
        if !matches!(
            self.shell_path.file_name().and_then(|name| name.to_str()),
            Some("zsh" | "bash" | "sh" | "dash")
        ) {
            return Err(Error::Configuration(
                "shell_path must point to zsh, bash, sh, or dash".into(),
            ));
        }
        if let Some(url) = &self.server_url {
            let valid = if let Some(path) = url.strip_prefix("unix://") {
                Path::new(path).is_absolute()
            } else {
                reqwest::Url::parse(url).is_ok_and(|parsed| {
                    matches!(parsed.scheme(), "http" | "https")
                        && parsed.host_str().is_some()
                        && parsed.username().is_empty()
                        && parsed.password().is_none()
                        && parsed.query().is_none()
                        && parsed.fragment().is_none()
                })
            };
            if !valid {
                return Err(Error::Configuration(
                    "server_url must be an HTTP(S) base URL without credentials/query/fragment, or unix:// with an absolute path".into(),
                ));
            }
        }
        Ok(())
    }
}

pub fn load(path: &Path) -> Result<Config> {
    if !path.exists() {
        return Ok(Config::default());
    }
    reject_symlink(path)?;
    let contents = fs::read(path).map_err(|error| {
        Error::Configuration(format!("could not read {}: {error}", path.display()))
    })?;
    let config: Config = serde_json::from_slice(&contents).map_err(|error| {
        Error::Configuration(format!("could not parse {}: {error}", path.display()))
    })?;
    config.validate()?;
    Ok(config)
}

pub fn save(path: &Path, config: &Config) -> Result<()> {
    config.validate()?;
    if path.exists() {
        reject_symlink(path)?;
    }
    let parent = path.parent().ok_or_else(|| {
        Error::Configuration(format!("config path {} has no parent", path.display()))
    })?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".config.json.{}.{:016x}.tmp",
        std::process::id(),
        rand::random::<u64>()
    ));
    let data = serde_json::to_vec_pretty(config)?;
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(&data)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    FileSync::sync_directory(parent)?;
    Ok(())
}

struct FileSync;

impl FileSync {
    #[cfg(unix)]
    fn sync_directory(path: &Path) -> Result<()> {
        fs::File::open(path)?.sync_all()?;
        Ok(())
    }

    #[cfg(not(unix))]
    fn sync_directory(_path: &Path) -> Result<()> {
        Ok(())
    }
}

fn reject_symlink(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(Error::Configuration(format!(
            "refusing to use symlinked config file {}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{load, save, Config};

    #[test]
    fn missing_config_uses_defaults() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            load(&directory.path().join("missing.json")).unwrap(),
            Config::default()
        );
    }

    #[test]
    fn existing_schema_one_config_defaults_new_preferences() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        std::fs::write(&path, br#"{"schema":1}"#).unwrap();
        assert!(load(&path).unwrap().show_tab_hint);
    }

    #[test]
    fn save_round_trips() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let config = Config {
            threads: 2,
            ..Config::default()
        };
        save(&path, &config).unwrap();
        assert_eq!(load(&path).unwrap(), config);
    }

    #[test]
    fn future_schema_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        std::fs::write(&path, br#"{"schema":999}"#).unwrap();
        assert!(load(&path).unwrap_err().to_string().contains("newer"));
    }

    #[test]
    fn older_schema_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        std::fs::write(&path, br#"{"schema":0}"#).unwrap();
        assert!(load(&path).unwrap_err().to_string().contains("older"));
    }

    #[test]
    fn unknown_config_fields_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        std::fs::write(&path, br#"{"schema":1,"threadz":4}"#).unwrap();
        assert!(load(&path)
            .unwrap_err()
            .to_string()
            .contains("unknown field"));
    }

    #[test]
    fn configured_binary_paths_must_be_absolute() {
        let config = Config {
            model_path: Some("model.gguf".into()),
            ..Config::default()
        };
        assert!(config.validate().is_err());

        let config = Config {
            llama_server_path: Some("llama-server".into()),
            ..Config::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn provider_urls_are_validated_as_base_urls() {
        for url in [
            "https://provider.example",
            "http://127.0.0.1:8080/api",
            "unix:///tmp/provider.sock",
        ] {
            let config = Config {
                server_url: Some(url.into()),
                ..Config::default()
            };
            assert!(config.validate().is_ok(), "{url}");
        }
        for url in [
            "https://",
            "https://user:secret@provider.example",
            "https://provider.example?token=secret",
            "unix://relative.sock",
        ] {
            let config = Config {
                server_url: Some(url.into()),
                ..Config::default()
            };
            assert!(config.validate().is_err(), "{url}");
        }
    }

    #[test]
    fn generation_budget_must_fit_inside_context() {
        let config = Config {
            context_size: 256,
            max_tokens: 256,
            ..Config::default()
        };
        assert!(config.validate().is_err());
    }
}
