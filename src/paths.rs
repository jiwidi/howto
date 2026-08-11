use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub runtime_dir: PathBuf,
}

impl Paths {
    pub fn discover() -> Result<Self> {
        if let Some(root) = env::var_os("HOWTO_HOME").filter(|value| !value.is_empty()) {
            return Self::under_override(PathBuf::from(root));
        }

        let base = dirs::home_dir().ok_or_else(|| {
            Error::Configuration("could not determine the current user's home directory".into())
        })?;

        #[cfg(target_os = "macos")]
        let paths = Self {
            config_dir: base.join("Library/Application Support/HowTo"),
            data_dir: base.join("Library/Application Support/HowTo"),
            cache_dir: base.join("Library/Caches/HowTo"),
            logs_dir: base.join("Library/Logs/HowTo"),
            runtime_dir: runtime_directory(&base)?,
        };

        #[cfg(not(target_os = "macos"))]
        let paths = {
            let config_home =
                environment_directory("XDG_CONFIG_HOME")?.unwrap_or_else(|| base.join(".config"));
            let data_home = environment_directory("XDG_DATA_HOME")?
                .unwrap_or_else(|| base.join(".local/share"));
            let cache_home =
                environment_directory("XDG_CACHE_HOME")?.unwrap_or_else(|| base.join(".cache"));
            let state_home = environment_directory("XDG_STATE_HOME")?
                .unwrap_or_else(|| base.join(".local/state"));
            Self {
                config_dir: config_home.join("howto"),
                data_dir: data_home.join("howto"),
                cache_dir: cache_home.join("howto"),
                logs_dir: state_home.join("howto/log"),
                runtime_dir: runtime_directory(&base)?,
            }
        };

        Ok(paths)
    }

    #[must_use]
    pub fn under(root: PathBuf) -> Self {
        Self {
            config_dir: root.clone(),
            data_dir: root.clone(),
            cache_dir: root.join("cache"),
            logs_dir: root.join("logs"),
            runtime_dir: root.join("run"),
        }
    }

    fn under_override(root: PathBuf) -> Result<Self> {
        if !root.is_absolute() {
            return Err(Error::Configuration(
                "HOWTO_HOME must be an absolute path".into(),
            ));
        }
        Ok(Self::under(root))
    }

    pub fn create(&self) -> Result<()> {
        for directory in [
            &self.config_dir,
            &self.data_dir,
            &self.cache_dir,
            &self.logs_dir,
            &self.runtime_dir,
            &self.models_dir(),
        ] {
            create_private_directory(directory)?;
        }
        Ok(())
    }

    #[must_use]
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.json")
    }

    #[must_use]
    pub fn models_dir(&self) -> PathBuf {
        self.data_dir.join("models")
    }

    #[must_use]
    pub fn model_lock_file(&self) -> PathBuf {
        self.models_dir().join(".install.lock")
    }

    #[must_use]
    pub fn server_state_file(&self) -> PathBuf {
        self.runtime_dir.join("server.json")
    }

    #[must_use]
    pub fn server_lock_file(&self) -> PathBuf {
        self.runtime_dir.join("server.lock")
    }

    #[must_use]
    pub fn server_key_file(&self) -> PathBuf {
        self.runtime_dir.join("server.key")
    }

    #[must_use]
    pub fn server_socket_file(&self) -> PathBuf {
        self.runtime_dir.join("server.sock")
    }

    #[must_use]
    pub fn server_log_file(&self) -> PathBuf {
        self.logs_dir.join("llama-server.log")
    }
}

fn environment_directory(name: &str) -> Result<Option<PathBuf>> {
    let Some(value) = env::var_os(name).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(Error::Configuration(format!(
            "{name} must be an absolute path when set"
        )));
    }
    Ok(Some(path))
}

fn runtime_directory(home: &Path) -> Result<PathBuf> {
    if let Some(runtime) = environment_directory("XDG_RUNTIME_DIR")? {
        return Ok(runtime.join("howto"));
    }
    fallback_runtime_directory(env::temp_dir(), home)
}

fn fallback_runtime_directory(temporary: PathBuf, home: &Path) -> Result<PathBuf> {
    if !temporary.is_absolute() {
        return Err(Error::Configuration(
            "TMPDIR must be an absolute path when used for runtime state".into(),
        ));
    }
    Ok(temporary.join(format!("howto-{}", current_uid(home))))
}

#[cfg(unix)]
fn current_uid(_home: &Path) -> u32 {
    nix::unistd::Uid::current().as_raw()
}

#[cfg(not(unix))]
fn current_uid(home: &Path) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(
        home.as_os_str().to_string_lossy().as_bytes(),
    ))[..12]
        .to_owned()
}

fn create_private_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(Error::Configuration(format!(
                "refusing unsafe data directory {}",
                path.display()
            )));
        }
        if metadata.uid() != nix::unistd::Uid::current().as_raw() {
            return Err(Error::Configuration(format!(
                "data directory is owned by another user: {}",
                path.display()
            )));
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{fallback_runtime_directory, Paths};

    #[test]
    fn override_layout_is_predictable() {
        let paths = Paths::under("/tmp/howto-test".into());
        assert_eq!(
            paths.config_file(),
            PathBuf::from("/tmp/howto-test/config.json")
        );
        assert_eq!(paths.models_dir(), PathBuf::from("/tmp/howto-test/models"));
        assert_eq!(
            paths.server_state_file(),
            PathBuf::from("/tmp/howto-test/run/server.json")
        );
    }

    #[test]
    fn override_root_must_be_absolute() {
        assert!(Paths::under_override(PathBuf::from(".howto")).is_err());
        assert!(Paths::under_override(PathBuf::from("/tmp/howto-test")).is_ok());
    }

    #[test]
    fn fallback_runtime_root_must_be_absolute() {
        let home = PathBuf::from("/home/howto-test");
        let error = fallback_runtime_directory(PathBuf::from("relative-tmp"), &home)
            .unwrap_err()
            .to_string();
        assert!(error.contains("TMPDIR must be an absolute path"));

        let runtime = fallback_runtime_directory(PathBuf::from("/tmp"), &home).unwrap();
        assert!(runtime.is_absolute());
        assert_eq!(runtime.parent(), Some(std::path::Path::new("/tmp")));
    }
}
