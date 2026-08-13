use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};
use crate::paths::Paths;

const SETUP_SCHEMA: u32 = 1;
pub const SHELL_INTEGRATION_SCHEMA: u32 = 2;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub schema: u32,
    pub completed_at: u64,
    pub shell: Option<String>,
    pub shell_integration_schema: Option<u32>,
    #[serde(default)]
    pub shell_startup_files: Vec<PathBuf>,
    #[serde(default)]
    pub failed_command_advisor_prompted: bool,
}

impl Receipt {
    #[must_use]
    pub fn new(shell: Option<&str>) -> Self {
        Self {
            schema: SETUP_SCHEMA,
            completed_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            shell: shell.map(str::to_owned),
            shell_integration_schema: shell.map(|_| SHELL_INTEGRATION_SCHEMA),
            shell_startup_files: Vec::new(),
            failed_command_advisor_prompted: false,
        }
    }

    pub fn set_shell(&mut self, shell: Option<&str>, startup_files: Vec<PathBuf>) {
        self.shell = shell.map(str::to_owned);
        self.shell_integration_schema = shell.map(|_| SHELL_INTEGRATION_SCHEMA);
        self.shell_startup_files = startup_files;
    }

    fn validate(&self) -> Result<()> {
        if self.schema != SETUP_SCHEMA {
            return Err(Error::Configuration(format!(
                "setup state schema {} is not supported (expected {SETUP_SCHEMA})",
                self.schema
            )));
        }
        if self.completed_at == 0 {
            return Err(Error::Configuration(
                "setup state has an invalid completion time".into(),
            ));
        }
        if self
            .shell
            .as_deref()
            .is_some_and(|shell| !matches!(shell, "zsh" | "bash" | "fish"))
        {
            return Err(Error::Configuration(
                "setup state contains an unsupported shell".into(),
            ));
        }
        let integration_is_supported = match (&self.shell, self.shell_integration_schema) {
            (None, None) => true,
            (Some(_), Some(schema)) => (1..=SHELL_INTEGRATION_SCHEMA).contains(&schema),
            _ => false,
        };
        if !integration_is_supported {
            return Err(Error::Configuration(
                "setup state contains inconsistent shell integration data".into(),
            ));
        }
        if self.shell.is_none() && !self.shell_startup_files.is_empty() {
            return Err(Error::Configuration(
                "setup state contains startup files without a shell integration".into(),
            ));
        }
        if self
            .shell_startup_files
            .iter()
            .any(|path| !path.is_absolute())
        {
            return Err(Error::Configuration(
                "setup state contains a relative shell startup path".into(),
            ));
        }
        Ok(())
    }
}

pub struct Lock {
    _file: File,
}

impl Lock {
    pub fn acquire(paths: &Paths) -> Result<Self> {
        paths.create()?;
        let path = paths.setup_lock_file();
        reject_unsafe_file_if_present(&path)?;
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};

            let metadata = file.metadata()?;
            if !metadata.is_file() || metadata.uid() != nix::unistd::Uid::current().as_raw() {
                return Err(Error::Configuration(format!(
                    "refusing unsafe setup lock {}",
                    path.display()
                )));
            }
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        FileExt::lock_exclusive(&file)?;
        Ok(Self { _file: file })
    }
}

pub fn load(paths: &Paths) -> Result<Option<Receipt>> {
    let path = paths.setup_state_file();
    let Some(bytes) = read_setup_state(&path)? else {
        return Ok(None);
    };
    let receipt = parse_receipt(&path, &bytes)?;
    receipt.validate()?;
    Ok(Some(receipt))
}

pub fn load_for_setup(paths: &Paths) -> Result<(Option<Receipt>, Option<std::path::PathBuf>)> {
    let path = paths.setup_state_file();
    let Some(bytes) = read_setup_state(&path)? else {
        return Ok((None, None));
    };
    let value = match serde_json::from_slice::<Value>(&bytes) {
        Ok(value) => value,
        Err(_) => return quarantine_malformed_state(&path),
    };
    let future_schema = value
        .get("schema")
        .and_then(Value::as_u64)
        .is_some_and(|schema| schema > u64::from(SETUP_SCHEMA));
    let future_integration = value
        .get("shell_integration_schema")
        .and_then(Value::as_u64)
        .is_some_and(|schema| schema > u64::from(SHELL_INTEGRATION_SCHEMA));
    if future_schema || future_integration {
        let receipt = parse_receipt(&path, &bytes)?;
        receipt.validate()?;
        return Ok((Some(receipt), None));
    }
    match parse_receipt(&path, &bytes).and_then(|receipt| {
        receipt.validate()?;
        Ok(receipt)
    }) {
        Ok(receipt) => Ok((Some(receipt), None)),
        Err(_) => quarantine_malformed_state(&path),
    }
}

fn read_setup_state(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(Error::Configuration(format!(
                    "refusing unsafe setup state {}",
                    path.display()
                )));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if metadata.uid() != nix::unistd::Uid::current().as_raw() {
                    return Err(Error::Configuration(format!(
                        "setup state is owned by another user: {}",
                        path.display()
                    )));
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    Ok(Some(fs::read(path)?))
}

fn parse_receipt(path: &Path, bytes: &[u8]) -> Result<Receipt> {
    serde_json::from_slice(bytes).map_err(|error| {
        Error::Configuration(format!("could not parse {}: {error}", path.display()))
    })
}

fn quarantine_malformed_state(
    path: &Path,
) -> Result<(Option<Receipt>, Option<std::path::PathBuf>)> {
    let quarantined = path.with_extension(format!(
        "json.corrupt-{}-{}-{:08x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        std::process::id(),
        rand::random::<u32>()
    ));
    fs::rename(path, &quarantined).map_err(|error| {
        Error::Configuration(format!(
            "could not quarantine malformed setup state {}: {error}",
            path.display()
        ))
    })?;
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok((None, Some(quarantined)))
}

pub fn save(paths: &Paths, receipt: &Receipt) -> Result<()> {
    receipt.validate()?;
    paths.create()?;
    let path = paths.setup_state_file();
    reject_unsafe_file_if_present(&path)?;
    let parent = path.parent().ok_or_else(|| {
        Error::Configuration(format!("setup path {} has no parent", path.display()))
    })?;
    let temporary = parent.join(format!(
        ".setup.json.{}.{:016x}.tmp",
        std::process::id(),
        rand::random::<u64>()
    ));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    let data = serde_json::to_vec_pretty(receipt)?;
    let result = (|| -> Result<()> {
        file.write_all(&data)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temporary, &path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn reject_unsafe_file_if_present(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(
            Error::Configuration(format!("refusing unsafe file {}", path.display())),
        ),
        Ok(metadata) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if metadata.uid() != nix::unistd::Uid::current().as_raw() {
                    return Err(Error::Configuration(format!(
                        "refusing file owned by another user: {}",
                        path.display()
                    )));
                }
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{load, load_for_setup, save, Receipt};
    use crate::paths::Paths;

    #[test]
    fn missing_receipt_means_setup_is_incomplete() {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::under(directory.path().to_path_buf());
        assert_eq!(load(&paths).unwrap(), None);
    }

    #[test]
    fn receipt_round_trips() {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::under(directory.path().to_path_buf());
        let receipt = Receipt::new(Some("zsh"));
        save(&paths, &receipt).unwrap();
        assert_eq!(load(&paths).unwrap(), Some(receipt));
    }

    #[test]
    fn older_receipt_defaults_beta_consent_to_not_prompted() {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::under(directory.path().to_path_buf());
        paths.create().unwrap();
        fs::write(
            paths.setup_state_file(),
            br#"{"schema":1,"completed_at":1,"shell":"zsh","shell_integration_schema":1,"shell_startup_files":[]}"#,
        )
        .unwrap();
        let receipt = load(&paths).unwrap().unwrap();
        assert!(!receipt.failed_command_advisor_prompted);
    }

    #[test]
    fn receipt_requires_absolute_owned_startup_paths() {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::under(directory.path().to_path_buf());
        let mut receipt = Receipt::new(Some("zsh"));
        receipt.shell_startup_files.push(".zshrc".into());
        assert!(save(&paths, &receipt).is_err());

        let mut without_shell = Receipt::new(None);
        without_shell
            .shell_startup_files
            .push(directory.path().join(".zshrc"));
        assert!(save(&paths, &without_shell).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn receipt_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::under(directory.path().to_path_buf());
        save(&paths, &Receipt::new(None)).unwrap();
        assert_eq!(
            fs::metadata(paths.setup_state_file())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn malformed_and_future_receipts_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::under(directory.path().to_path_buf());
        paths.create().unwrap();
        fs::write(paths.setup_state_file(), b"not json").unwrap();
        assert!(load(&paths).is_err());
        fs::write(
            paths.setup_state_file(),
            br#"{"schema":2,"completed_at":1,"shell":null,"shell_integration_schema":null}"#,
        )
        .unwrap();
        assert!(load(&paths).is_err());
        fs::write(
            paths.setup_state_file(),
            br#"{"schema":1,"completed_at":1,"shell":"zsh","shell_integration_schema":3}"#,
        )
        .unwrap();
        assert!(load(&paths).is_err());
        fs::write(
            paths.setup_state_file(),
            br#"{"schema":1,"completed_at":1,"shell":null,"shell_integration_schema":3}"#,
        )
        .unwrap();
        assert!(load(&paths).is_err());
    }

    #[test]
    fn explicit_setup_quarantines_malformed_state_but_not_future_state() {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::under(directory.path().to_path_buf());
        paths.create().unwrap();
        fs::write(paths.setup_state_file(), b"{truncated").unwrap();
        let (receipt, quarantined) = load_for_setup(&paths).unwrap();
        assert_eq!(receipt, None);
        assert!(quarantined.unwrap().is_file());
        assert!(!paths.setup_state_file().exists());

        fs::write(
            paths.setup_state_file(),
            br#"{"completed_at":1,"shell":null,"shell_integration_schema":null}"#,
        )
        .unwrap();
        let (receipt, quarantined) = load_for_setup(&paths).unwrap();
        assert_eq!(receipt, None);
        assert!(quarantined.unwrap().is_file());

        fs::write(
            paths.setup_state_file(),
            br#"{"schema":2,"completed_at":1,"shell":null,"shell_integration_schema":null}"#,
        )
        .unwrap();
        assert!(load_for_setup(&paths).is_err());
        assert!(paths.setup_state_file().is_file());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_receipt_is_rejected() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::under(directory.path().to_path_buf());
        paths.create().unwrap();
        let target = directory.path().join("target");
        fs::write(&target, b"{}").unwrap();
        symlink(target, paths.setup_state_file()).unwrap();
        assert!(load(&paths).is_err());
    }
}
