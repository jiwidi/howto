use std::env;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::paths::Paths;

pub const SESSION_ENV: &str = "HOWTO_SHELL_SESSION";
const PENDING_SCHEMA: u32 = 1;
const PENDING_TTL: Duration = Duration::from_secs(10 * 60);
const BLOCK_START: &str = "# >>> HowTo shell integration >>>";
const BLOCK_END: &str = "# <<< HowTo shell integration <<<";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Zsh,
    Bash,
    Fish,
}

impl Kind {
    pub const ALL: [Self; 3] = [Self::Zsh, Self::Bash, Self::Fish];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Zsh => "zsh",
            Self::Bash => "bash",
            Self::Fish => "fish",
        }
    }

    #[must_use]
    pub const fn adapter(self) -> &'static str {
        match self {
            Self::Zsh => include_str!("../shell/howto.zsh"),
            Self::Bash => include_str!("../shell/howto.bash"),
            Self::Fish => include_str!("../shell/howto.fish"),
        }
    }

    const fn adapter_file_name(self) -> &'static str {
        match self {
            Self::Zsh => "howto.zsh",
            Self::Bash => "howto.bash",
            Self::Fish => "howto.fish",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

impl FromStr for Kind {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "zsh" => Ok(Self::Zsh),
            "bash" => Ok(Self::Bash),
            "fish" => Ok(Self::Fish),
            _ => Err(Error::Usage(format!(
                "unsupported shell `{value}` (expected zsh, bash, or fish)"
            ))),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnableResult {
    pub adapter_file: PathBuf,
    pub startup_files: Vec<PathBuf>,
    pub changed: bool,
    pub backup_files: Vec<PathBuf>,
    previous: ShellSnapshot,
}

/// A symbolic link in a startup path that must be approved before HowTo follows it.
///
/// Instances are produced by [`startup_symlink_requests`]. Keeping the fields
/// private ensures callers cannot accidentally manufacture an approval without
/// first resolving and validating the link through this module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StartupSymlink {
    link_path: PathBuf,
    target_path: PathBuf,
    managed_block: String,
}

impl StartupSymlink {
    #[must_use]
    pub fn link_path(&self) -> &Path {
        &self.link_path
    }

    #[must_use]
    pub fn target_path(&self) -> &Path {
        &self.target_path
    }

    #[must_use]
    pub fn managed_block(&self) -> &str {
        &self.managed_block
    }
}

impl EnableResult {
    pub fn rollback(&self) -> Result<()> {
        if let Err(error) = restore_snapshot(&self.previous) {
            return Err(Error::Configuration(format!(
                "{error}; transaction backups were preserved for manual recovery"
            )));
        }
        remove_backup_files(&self.backup_files)
    }
}

fn remove_backup_files(backup_files: &[PathBuf]) -> Result<()> {
    let mut errors = Vec::new();
    for backup in backup_files {
        match fs::remove_file(backup) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => errors.push(format!("{}: {error}", backup.display())),
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(Error::Configuration(format!(
            "could not remove shell startup backups: {}",
            errors.join("; ")
        )))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ShellSnapshot {
    adapter_file: PathBuf,
    adapter: Option<Vec<u8>>,
    startup_files: Vec<(PathBuf, Option<String>)>,
}

pub fn detect() -> Result<Kind> {
    if let Some(shell) = env::var_os("SHELL") {
        let name = Path::new(&shell)
            .file_name()
            .and_then(|value| value.to_str());
        if let Some(name @ ("zsh" | "bash" | "fish")) = name {
            return name.parse();
        }
        return Err(Error::Dependency(format!(
            "the current login shell `{}` is not supported for Tab integration; use `--shell zsh`, `--shell bash`, `--shell fish`, or `--no-shell`",
            Path::new(&shell).display()
        )));
    }
    if cfg!(target_os = "macos") {
        Ok(Kind::Zsh)
    } else {
        Ok(Kind::Bash)
    }
}

pub fn ensure_supported(kind: Kind) -> Result<()> {
    if kind != Kind::Bash {
        return Ok(());
    }
    let candidates = find_shells(kind);
    if candidates.is_empty() {
        return Err(Error::Dependency(
            "bash was not found; install Bash 4.1 or newer".into(),
        ));
    }
    for executable in &candidates {
        if bash_version_at_least(executable, 4, 1) {
            return Ok(());
        }
    }
    Err(Error::Dependency(format!(
        "{} is too old for context-aware Tab integration, and no newer bash was found in PATH; Bash 4.1 or newer is required",
        candidates[0].display()
    )))
}

/// Returns whether the installed adapter can provide failed-command advice for
/// this shell. Tab insertion has a lower Bash requirement and remains usable
/// on Bash 4.1 through 5.0.
#[must_use]
pub fn failed_command_advisor_supported(kind: Kind) -> bool {
    kind != Kind::Bash
        || find_shells(kind)
            .iter()
            .any(|executable| bash_version_at_least(executable, 5, 1))
}

fn bash_version_at_least(executable: &Path, major: u16, minor: u16) -> bool {
    Command::new(executable)
        .args([
            "--noprofile",
            "--norc",
            "-c",
            &format!(
                "(( BASH_VERSINFO[0] > {major} || (BASH_VERSINFO[0] == {major} && BASH_VERSINFO[1] >= {minor}) ))"
            ),
        ])
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

pub fn has_managed_artifacts(paths: &Paths, kind: Kind) -> Result<bool> {
    let adapter = paths.shell_dir().join(kind.adapter_file_name());
    match fs::symlink_metadata(&adapter) {
        Ok(_) => return Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(!marker_startup_files(kind)?.is_empty())
}

pub fn enable(paths: &Paths, kind: Kind) -> Result<EnableResult> {
    enable_recorded(paths, kind, &[])
}

pub fn enable_recorded(
    paths: &Paths,
    kind: Kind,
    recorded_startup_files: &[PathBuf],
) -> Result<EnableResult> {
    enable_recorded_approved(paths, kind, recorded_startup_files, &[])
}

/// Enables shell integration after the caller has explicitly approved every
/// symlink returned by [`startup_symlink_requests`].
///
/// The links are resolved again and must still point at the exact approved
/// targets. This prevents a stale prompt from authorizing a different file.
pub fn enable_recorded_approved(
    paths: &Paths,
    kind: Kind,
    recorded_startup_files: &[PathBuf],
    approved_symlinks: &[StartupSymlink],
) -> Result<EnableResult> {
    ensure_supported(kind)?;
    paths.create()?;
    let adapter_file = paths.shell_dir().join(kind.adapter_file_name());
    let block = managed_block(kind, &adapter_file);
    let startup_files =
        resolve_startup_files(kind, &block, recorded_startup_files, approved_symlinks)?;
    let mut stale_startup_files = recorded_startup_files.to_vec();
    extend_unique(&mut stale_startup_files, marker_startup_files(kind)?);
    stale_startup_files.retain(|path| !startup_files.contains(path));
    let mut affected_startup_files = startup_files.clone();
    extend_unique(&mut affected_startup_files, stale_startup_files.clone());
    let previous = snapshot_shell_state(&adapter_file, &affected_startup_files)?;
    let adapter_changed = previous.adapter.as_deref() != Some(kind.adapter().as_bytes());
    if let Err(error) = write_private_atomic(&adapter_file, kind.adapter().as_bytes()) {
        return Err(rollback_shell_update(error, &previous, &[]));
    }

    let enabled = match update_startup_files(&startup_files, Some(&block)) {
        Ok(update) => update,
        Err(error) => {
            return Err(rollback_shell_update(error, &previous, &[]));
        }
    };
    let cleaned = match update_startup_files(&stale_startup_files, None) {
        Ok(update) => update,
        Err(error) => {
            return Err(rollback_shell_update(
                error,
                &previous,
                &enabled.backup_files,
            ));
        }
    };
    let mut backup_files = enabled.backup_files;
    backup_files.extend(cleaned.backup_files);
    Ok(EnableResult {
        adapter_file,
        startup_files,
        changed: adapter_changed || enabled.changed || cleaned.changed,
        backup_files,
        previous,
    })
}

/// Returns validated symbolic-link startup paths that need separate user approval.
/// Inspecting them is read-only; modification remains impossible unless these
/// exact values are passed to [`enable_recorded_approved`].
pub fn startup_symlink_requests(
    paths: &Paths,
    kind: Kind,
    recorded_startup_files: &[PathBuf],
) -> Result<Vec<StartupSymlink>> {
    let adapter_file = paths.shell_dir().join(kind.adapter_file_name());
    let block = managed_block(kind, &adapter_file);
    let mut requests = Vec::new();
    for link_path in startup_files(kind)? {
        if let Some(target_path) = redirected_startup_target(&link_path)? {
            if recorded_startup_files.contains(&target_path) {
                continue;
            }
            requests.push(StartupSymlink {
                link_path,
                target_path,
                managed_block: block.clone(),
            });
        }
    }
    Ok(requests)
}

pub fn disable_recorded(
    paths: &Paths,
    kind: Kind,
    recorded_startup_files: &[PathBuf],
) -> Result<EnableResult> {
    let mut startup_files = recorded_startup_files.to_vec();
    match marker_startup_files(kind) {
        Ok(extra) => extend_unique(&mut startup_files, extra),
        Err(error) if startup_files.is_empty() => return Err(error),
        Err(_) => {}
    }
    disable_from_startup_files(paths, kind, startup_files)
}

pub fn refresh_adapter(paths: &Paths, kind: Kind) -> Result<bool> {
    paths.create()?;
    let adapter_file = paths.shell_dir().join(kind.adapter_file_name());
    match fs::symlink_metadata(&adapter_file) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(Error::Configuration(format!(
                "refusing unsafe shell adapter {}",
                adapter_file.display()
            )))
        }
        Ok(_) => {
            if fs::read(&adapter_file)? == kind.adapter().as_bytes() {
                return Ok(false);
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    write_private_atomic(&adapter_file, kind.adapter().as_bytes())?;
    Ok(true)
}

pub fn disable(paths: &Paths, kind: Kind) -> Result<EnableResult> {
    disable_recorded(paths, kind, &[])
}

fn disable_from_startup_files(
    paths: &Paths,
    kind: Kind,
    startup_files: Vec<PathBuf>,
) -> Result<EnableResult> {
    let adapter_file = paths.shell_dir().join(kind.adapter_file_name());
    let previous = snapshot_shell_state(&adapter_file, &startup_files)?;
    let adapter_changed = previous.adapter.is_some();
    purge_pending(paths)?;
    remove_regular_file_if_present(&adapter_file)?;
    let update = match update_startup_files(&startup_files, None) {
        Ok(update) => update,
        Err(error) => {
            return Err(rollback_shell_update(error, &previous, &[]));
        }
    };
    Ok(EnableResult {
        adapter_file,
        startup_files,
        changed: adapter_changed || update.changed,
        backup_files: update.backup_files,
        previous,
    })
}

#[must_use]
pub fn is_enabled(paths: &Paths, kind: Kind) -> bool {
    is_enabled_at(paths, kind, &[])
}

#[must_use]
pub fn is_enabled_at(paths: &Paths, kind: Kind, recorded_startup_files: &[PathBuf]) -> bool {
    let adapter_file = paths.shell_dir().join(kind.adapter_file_name());
    let block = managed_block(kind, &adapter_file);
    let Ok(current_startup_files) = effective_startup_files(kind) else {
        return false;
    };
    let startup_files = if recorded_startup_files.is_empty() {
        current_startup_files
    } else if same_paths(&current_startup_files, recorded_startup_files) {
        recorded_startup_files.to_vec()
    } else {
        return false;
    };
    if startup_files.is_empty() {
        return false;
    }
    is_regular_file(&adapter_file)
        && fs::read(&adapter_file).is_ok_and(|contents| contents == kind.adapter().as_bytes())
        && startup_files.iter().all(|startup_file| {
            is_regular_file(startup_file)
                && fs::read_to_string(startup_file).is_ok_and(|contents| {
                    contents.matches(BLOCK_START).count() == 1
                        && contents.matches(BLOCK_END).count() == 1
                        && contents.contains(&block)
                })
        })
}

fn managed_block(kind: Kind, adapter_file: &Path) -> String {
    let source_line = match kind {
        Kind::Fish => format!("test -r {0}; and source {0}", quote(kind, adapter_file)),
        Kind::Zsh => format!(
            "if [ -r {0} ]; then\n  . {0}\nfi",
            quote(kind, adapter_file)
        ),
        Kind::Bash => format!(
            "if [ -n \"${{BASH_VERSION:-}}\" ] && [ -r {0} ]; then\n  . {0}\nfi",
            quote(kind, adapter_file)
        ),
    };
    format!("{BLOCK_START}\n{source_line}\n{BLOCK_END}\n")
}

fn is_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
}

pub fn store_pending(paths: &Paths, command: &str) -> Result<bool> {
    let Some((session, session_hash)) = session()? else {
        return Ok(false);
    };
    validate_command(command)?;
    paths.create()?;
    cleanup_expired_pending(paths)?;
    let pending = Pending {
        schema: PENDING_SCHEMA,
        created_at: now_seconds(),
        session,
        command: command.to_owned(),
    };
    let data = serde_json::to_vec(&pending)?;
    write_private_atomic(&pending_file(paths, &session_hash), &data)?;
    Ok(true)
}

pub fn take_pending(paths: &Paths) -> Result<Option<String>> {
    let Some((session, session_hash)) = session()? else {
        return Ok(None);
    };
    paths.create()?;
    cleanup_expired_pending(paths)?;
    let path = pending_file(paths, &session_hash);
    let claimed = paths.runtime_dir.join(format!(
        ".pending-claim-{}-{:016x}.json",
        std::process::id(),
        rand::random::<u64>()
    ));
    match fs::rename(&path, &claimed) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let result = (|| -> Result<Option<String>> {
        let metadata = fs::symlink_metadata(&claimed)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(Error::Configuration(format!(
                "refusing unsafe pending command {}",
                claimed.display()
            )));
        }
        let pending: Pending = serde_json::from_slice(&fs::read(&claimed)?).map_err(|error| {
            Error::Configuration(format!("could not parse pending HowTo command: {error}"))
        })?;
        if pending.schema != PENDING_SCHEMA || pending.session != session {
            return Err(Error::Configuration(
                "pending HowTo command has invalid state".into(),
            ));
        }
        validate_command(&pending.command)?;
        if pending_age(pending.created_at).is_none() {
            return Ok(None);
        }
        Ok(Some(pending.command))
    })();
    let removal = fs::remove_file(&claimed);
    match (result, removal) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error.into()),
        (Ok(command), Ok(())) => Ok(command),
    }
}

pub fn discard_pending(paths: &Paths) -> Result<bool> {
    let Some((_, session_hash)) = session()? else {
        return Ok(false);
    };
    paths.create()?;
    cleanup_expired_pending(paths)?;
    match fs::remove_file(pending_file(paths, &session_hash)) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    schema: u32,
    created_at: u64,
    session: String,
    command: String,
}

struct StartupUpdate {
    changed: bool,
    backup_files: Vec<PathBuf>,
}

struct StartupPlan {
    path: PathBuf,
    existing: Option<String>,
    updated: String,
}

fn snapshot_shell_state(adapter_file: &Path, startup_files: &[PathBuf]) -> Result<ShellSnapshot> {
    let adapter = read_regular_file_if_present(adapter_file)?;
    let startup_files = startup_files
        .iter()
        .map(|path| {
            let plan = plan_startup_file(path, None)?;
            Ok((path.clone(), plan.existing))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ShellSnapshot {
        adapter_file: adapter_file.to_path_buf(),
        adapter,
        startup_files,
    })
}

fn restore_snapshot(snapshot: &ShellSnapshot) -> Result<()> {
    let mut errors = Vec::new();
    if let Err(error) = restore_file(&snapshot.adapter_file, snapshot.adapter.as_deref()) {
        errors.push(format!("{}: {error}", snapshot.adapter_file.display()));
    }
    for (path, contents) in &snapshot.startup_files {
        let result = if let Some(contents) = contents {
            reject_unsafe_file_if_present(path)
                .and_then(|()| write_startup_atomic(path, contents.as_bytes()))
        } else {
            remove_regular_file_if_present(path)
        };
        if let Err(error) = result {
            errors.push(format!("{}: {error}", path.display()));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(Error::Configuration(format!(
            "could not restore shell files: {}",
            errors.join("; ")
        )))
    }
}

fn rollback_shell_update(error: Error, snapshot: &ShellSnapshot, backups: &[PathBuf]) -> Error {
    if let Err(rollback) = restore_snapshot(snapshot) {
        return Error::Configuration(format!(
            "{error}; restoring the previous shell integration failed: {rollback}; transaction backups were preserved for manual recovery"
        ));
    }
    match remove_backup_files(backups) {
        Ok(()) => error,
        Err(cleanup) => Error::Configuration(format!(
            "{error}; removing transaction backups also failed: {cleanup}"
        )),
    }
}

fn startup_files(kind: Kind) -> Result<Vec<PathBuf>> {
    let home = dirs::home_dir().ok_or_else(|| {
        Error::Configuration("could not determine the current user's home directory".into())
    })?;
    match kind {
        Kind::Zsh => {
            let directory = match env::var_os("ZDOTDIR").filter(|value| !value.is_empty()) {
                Some(value) => {
                    let path = PathBuf::from(value);
                    if !path.is_absolute() {
                        return Err(Error::Configuration(
                            "ZDOTDIR must be absolute to enable shell integration".into(),
                        ));
                    }
                    path
                }
                None => home,
            };
            Ok(vec![directory.join(".zshrc")])
        }
        Kind::Bash => {
            let login = [".bash_profile", ".bash_login", ".profile"]
                .into_iter()
                .map(|name| home.join(name))
                .find(|candidate| candidate.exists())
                .unwrap_or_else(|| home.join(".bash_profile"));
            Ok(vec![home.join(".bashrc"), login])
        }
        Kind::Fish => {
            let config_home = match env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty())
            {
                Some(value) => {
                    let path = PathBuf::from(value);
                    if !path.is_absolute() {
                        return Err(Error::Configuration(
                            "XDG_CONFIG_HOME must be absolute to enable shell integration".into(),
                        ));
                    }
                    path
                }
                None => home.join(".config"),
            };
            Ok(vec![config_home.join("fish/config.fish")])
        }
    }
}

fn resolve_startup_files(
    kind: Kind,
    block: &str,
    recorded_startup_files: &[PathBuf],
    approved_symlinks: &[StartupSymlink],
) -> Result<Vec<PathBuf>> {
    let mut resolved = Vec::new();
    for path in startup_files(kind)? {
        let effective_path = if let Some(target) = redirected_startup_target(&path)? {
            let approved = recorded_startup_files.contains(&target)
                || approved_symlinks.iter().any(|approval| {
                    approval.link_path == path
                        && approval.target_path == target
                        && approval.managed_block == block
                });
            if !approved {
                return Err(Error::Configuration(format!(
                    "shell startup path {} contains a symlink and resolves to {}; explicit approval is required before modifying the resolved target",
                    path.display(),
                    target.display()
                )));
            }
            target
        } else {
            path
        };
        if !resolved.contains(&effective_path) {
            resolved.push(effective_path);
        }
    }
    Ok(resolved)
}

fn effective_startup_files(kind: Kind) -> Result<Vec<PathBuf>> {
    let mut effective = Vec::new();
    for path in startup_files(kind)? {
        let path = redirected_startup_target(&path)?.unwrap_or(path);
        if !effective.contains(&path) {
            effective.push(path);
        }
    }
    Ok(effective)
}

fn same_paths(first: &[PathBuf], second: &[PathBuf]) -> bool {
    first.len() == second.len()
        && first.iter().all(|path| second.contains(path))
        && second.iter().all(|path| first.contains(path))
}

/// Resolves a startup path if its final component or any user-controlled
/// ancestor is a symlink. The returned path is canonical even when the final
/// startup file does not exist yet.
fn redirected_startup_target(path: &Path) -> Result<Option<PathBuf>> {
    let logical_home = dirs::home_dir().ok_or_else(|| {
        Error::Configuration("could not determine the current user's home directory".into())
    })?;
    let mut found_symlink = false;
    let mut leaf_is_symlink = false;

    // Ignore shared system ancestors of HOME (for example /var -> /private/var
    // on macOS), but inspect HOME itself, everything beneath it, and any
    // divergent path selected through ZDOTDIR/XDG_CONFIG_HOME.
    for candidate in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
        if !candidate.starts_with(&logical_home) && logical_home.starts_with(candidate) {
            continue;
        }
        match fs::symlink_metadata(candidate) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                #[cfg(unix)]
                ensure_current_user_owns(candidate, &metadata, "shell startup symlink")?;
                fs::canonicalize(candidate).map_err(|error| {
                    Error::Configuration(format!(
                        "could not resolve shell startup symlink {}: {error}",
                        candidate.display()
                    ))
                })?;
                found_symlink = true;
                leaf_is_symlink = candidate == path;
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    if !found_symlink {
        return Ok(None);
    }

    let target = match fs::canonicalize(path) {
        Ok(target) => target,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !leaf_is_symlink => {
            canonicalize_missing_path(path)?
        }
        Err(error) => {
            return Err(Error::Configuration(format!(
                "could not resolve shell startup symlink {}: {error}",
                path.display()
            )))
        }
    };
    validate_resolved_startup_target(path, &target)?;
    Ok(Some(target))
}

fn canonicalize_missing_path(path: &Path) -> Result<PathBuf> {
    let mut missing = Vec::new();
    let mut existing = path;
    loop {
        match fs::symlink_metadata(existing) {
            Ok(_) => {
                let mut resolved = fs::canonicalize(existing).map_err(|error| {
                    Error::Configuration(format!(
                        "could not resolve shell startup path {}: {error}",
                        path.display()
                    ))
                })?;
                for component in missing.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = existing.file_name().ok_or_else(|| {
                    Error::Configuration(format!(
                        "could not resolve shell startup path {}",
                        path.display()
                    ))
                })?;
                missing.push(name.to_os_string());
                existing = existing.parent().ok_or_else(|| {
                    Error::Configuration(format!(
                        "could not resolve shell startup path {}",
                        path.display()
                    ))
                })?;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn validate_resolved_startup_target(startup_path: &Path, target: &Path) -> Result<()> {
    let home = dirs::home_dir()
        .ok_or_else(|| {
            Error::Configuration("could not determine the current user's home directory".into())
        })?
        .canonicalize()
        .map_err(|error| {
            Error::Configuration(format!(
                "could not resolve the current user's home directory: {error}"
            ))
        })?;
    if !target.starts_with(&home) {
        return Err(Error::Configuration(format!(
            "shell startup symlink {} resolves outside the current user's home directory: {}",
            startup_path.display(),
            target.display()
        )));
    }
    match fs::symlink_metadata(target) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(Error::Configuration(format!(
                    "shell startup symlink {} does not resolve to a regular file: {}",
                    startup_path.display(),
                    target.display()
                )));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                ensure_current_user_owns(target, &metadata, "resolved shell startup file")?;
                if metadata.permissions().mode() & 0o022 != 0 {
                    return Err(Error::Configuration(format!(
                        "resolved shell startup file is writable by another user: {}",
                        target.display()
                    )));
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    #[cfg(unix)]
    validate_owned_home_path(&home, target)?;
    Ok(())
}

#[cfg(unix)]
fn ensure_current_user_owns(path: &Path, metadata: &fs::Metadata, description: &str) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    if metadata.uid() != nix::unistd::Uid::current().as_raw() {
        return Err(Error::Configuration(format!(
            "{description} is owned by another user: {}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn validate_owned_home_path(home: &Path, target: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut path = target.parent();
    while let Some(directory) = path {
        if !directory.starts_with(home) {
            break;
        }
        let metadata = match fs::symlink_metadata(directory) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                path = directory.parent();
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(Error::Configuration(format!(
                "resolved shell startup path contains an unsafe directory: {}",
                directory.display()
            )));
        }
        ensure_current_user_owns(directory, &metadata, "shell startup directory")?;
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err(Error::Configuration(format!(
                "shell startup directory is writable by another user: {}",
                directory.display()
            )));
        }
        if directory == home {
            break;
        }
        path = directory.parent();
    }
    Ok(())
}

fn known_startup_files(kind: Kind) -> Result<Vec<PathBuf>> {
    if kind != Kind::Bash {
        return startup_files(kind);
    }
    let home = dirs::home_dir().ok_or_else(|| {
        Error::Configuration("could not determine the current user's home directory".into())
    })?;
    Ok([".bashrc", ".bash_profile", ".bash_login", ".profile"]
        .into_iter()
        .map(|name| home.join(name))
        .collect())
}

fn marker_startup_files(kind: Kind) -> Result<Vec<PathBuf>> {
    let mut matches = Vec::new();
    for path in known_startup_files(kind)? {
        let candidate = match redirected_startup_target(&path) {
            Ok(Some(target)) => target,
            Ok(None) => path,
            // Discovery must not turn an unrelated unsafe link into a setup
            // failure. It also must never follow such a link.
            Err(_) => continue,
        };
        match fs::symlink_metadata(&candidate) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let contents = fs::read(&candidate).map_err(|error| {
            Error::Configuration(format!("could not read {}: {error}", candidate.display()))
        })?;
        if (contains_bytes(&contents, BLOCK_START.as_bytes())
            || contains_bytes(&contents, BLOCK_END.as_bytes()))
            && !matches.contains(&candidate)
        {
            matches.push(candidate);
        }
    }
    Ok(matches)
}

fn contains_bytes(contents: &[u8], needle: &[u8]) -> bool {
    contents
        .windows(needle.len())
        .any(|window| window == needle)
}

fn extend_unique(paths: &mut Vec<PathBuf>, extra: impl IntoIterator<Item = PathBuf>) {
    for path in extra {
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
}

fn update_startup_files(paths: &[PathBuf], block: Option<&str>) -> Result<StartupUpdate> {
    let plans = paths
        .iter()
        .map(|path| plan_startup_file(path, block))
        .collect::<Result<Vec<_>>>()?;
    let mut applied = Vec::new();
    let mut backup_files = Vec::new();
    for (index, plan) in plans.iter().enumerate() {
        if plan.existing.as_deref() == Some(plan.updated.as_str())
            || (plan.existing.is_none() && plan.updated.is_empty())
        {
            continue;
        }
        let mut current_backup = None;
        let update = (|| -> Result<()> {
            let parent = plan.path.parent().ok_or_else(|| {
                Error::Configuration(format!(
                    "shell startup path {} has no parent",
                    plan.path.display()
                ))
            })?;
            fs::create_dir_all(parent)?;
            if plan.existing.is_some() {
                let backup = plan.path.with_file_name(format!(
                    "{}.howto-backup-{}-{}-{:08x}",
                    plan.path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("shellrc"),
                    now_seconds(),
                    std::process::id(),
                    rand::random::<u32>()
                ));
                fs::copy(&plan.path, &backup)?;
                current_backup = Some(backup);
            }
            write_startup_atomic(&plan.path, plan.updated.as_bytes())?;
            Ok(())
        })();
        match update {
            Ok(()) => {
                applied.push(index);
                backup_files.extend(current_backup);
            }
            Err(error) => {
                let mut attempted = applied.clone();
                attempted.push(index);
                let rollback_error = rollback_startup_plans(&plans, &attempted);
                return if let Err(rollback) = rollback_error {
                    Err(Error::Configuration(format!(
                        "could not update shell startup files ({error}); rollback also failed ({rollback}); transaction backups were preserved for manual recovery"
                    )))
                } else {
                    for backup in &backup_files {
                        let _ = fs::remove_file(backup);
                    }
                    if let Some(backup) = current_backup {
                        let _ = fs::remove_file(backup);
                    }
                    Err(error)
                };
            }
        }
    }
    Ok(StartupUpdate {
        changed: !applied.is_empty(),
        backup_files,
    })
}

fn plan_startup_file(path: &Path, block: Option<&str>) -> Result<StartupPlan> {
    let existing = match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(Error::Configuration(format!(
                    "refusing to modify unsafe shell startup file {}",
                    path.display()
                )));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if metadata.uid() != nix::unistd::Uid::current().as_raw() {
                    return Err(Error::Configuration(format!(
                        "shell startup file is owned by another user: {}",
                        path.display()
                    )));
                }
            }
            Some(fs::read_to_string(path).map_err(|error| {
                Error::Configuration(format!("could not read {}: {error}", path.display()))
            })?)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let updated = replace_managed_block(existing.as_deref().unwrap_or_default(), block)?;
    Ok(StartupPlan {
        path: path.to_path_buf(),
        existing,
        updated,
    })
}

fn rollback_startup_plans(plans: &[StartupPlan], applied: &[usize]) -> Result<()> {
    let mut errors = Vec::new();
    for &index in applied.iter().rev() {
        let plan = &plans[index];
        let result = if let Some(existing) = &plan.existing {
            write_startup_atomic(&plan.path, existing.as_bytes())
        } else {
            remove_regular_file_if_present(&plan.path)
        };
        if let Err(error) = result {
            errors.push(format!("{}: {error}", plan.path.display()));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(Error::Configuration(errors.join("; ")))
    }
}

fn replace_managed_block(existing: &str, replacement: Option<&str>) -> Result<String> {
    let starts = existing.match_indices(BLOCK_START).collect::<Vec<_>>();
    let ends = existing.match_indices(BLOCK_END).collect::<Vec<_>>();
    if starts.len() != ends.len() || starts.len() > 1 {
        return Err(Error::Configuration(
            "shell startup file contains malformed or duplicate HowTo integration markers".into(),
        ));
    }
    if let (Some(&(start, _)), Some(&(end, _))) = (starts.first(), ends.first()) {
        if end < start {
            return Err(Error::Configuration(
                "shell startup file contains malformed HowTo integration markers".into(),
            ));
        }
        let mut block_end = end + BLOCK_END.len();
        if existing.as_bytes().get(block_end) == Some(&b'\n') {
            block_end += 1;
        }
        let prefix_end = if replacement.is_none() && existing[..start].ends_with("\n\n") {
            start - 1
        } else {
            start
        };
        let mut updated = String::with_capacity(existing.len() + replacement.map_or(0, str::len));
        updated.push_str(&existing[..prefix_end]);
        if let Some(replacement) = replacement {
            updated.push_str(replacement);
        }
        updated.push_str(&existing[block_end..]);
        return Ok(updated);
    }
    let Some(replacement) = replacement else {
        return Ok(existing.to_owned());
    };
    let mut updated = existing.to_owned();
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    if !updated.is_empty() {
        updated.push('\n');
    }
    updated.push_str(replacement);
    Ok(updated)
}

fn write_startup_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        Error::Configuration(format!(
            "shell startup path {} has no parent",
            path.display()
        ))
    })?;
    let mode = file_mode(path).unwrap_or(0o600);
    let temporary = parent.join(format!(
        ".howto-shell.{}.{:016x}.tmp",
        std::process::id(),
        rand::random::<u64>()
    ));
    write_new_file(&temporary, contents, mode)?;
    let result = fs::rename(&temporary, path).map_err(Error::from);
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn write_private_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    reject_unsafe_file_if_present(path)?;
    let parent = path.parent().ok_or_else(|| {
        Error::Configuration(format!("state path {} has no parent", path.display()))
    })?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".howto-state.{}.{:016x}.tmp",
        std::process::id(),
        rand::random::<u64>()
    ));
    write_new_file(&temporary, contents, 0o600)?;
    let result = fs::rename(&temporary, path).map_err(Error::from);
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn write_new_file(path: &Path, contents: &[u8], mode: u32) -> Result<()> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    let mut file = options.open(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    Ok(())
}

fn reject_unsafe_file_if_present(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(
            Error::Configuration(format!("refusing unsafe file {}", path.display())),
        ),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn read_regular_file_if_present(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(
            Error::Configuration(format!("refusing unsafe file {}", path.display())),
        ),
        Ok(_) => Ok(Some(fs::read(path)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn restore_file(path: &Path, contents: Option<&[u8]>) -> Result<()> {
    if let Some(contents) = contents {
        write_private_atomic(path, contents)
    } else {
        remove_regular_file_if_present(path)
    }
}

fn remove_regular_file_if_present(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(
            Error::Configuration(format!("refusing to remove unsafe file {}", path.display())),
        ),
        Ok(_) => {
            fs::remove_file(path)?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub fn session_kind() -> Result<Option<Kind>> {
    Ok(validated_session()?.map(|(kind, _)| kind))
}

fn session() -> Result<Option<(String, String)>> {
    let Some((_, value)) = validated_session()? else {
        return Ok(None);
    };
    let hash = hex::encode(Sha256::digest(value.as_bytes()));
    Ok(Some((value, hash)))
}

fn validated_session() -> Result<Option<(Kind, String)>> {
    let Some(value) = env::var_os(SESSION_ENV).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let value = value
        .into_string()
        .map_err(|_| Error::Configuration(format!("{SESSION_ENV} must contain valid UTF-8")))?;
    let kind = Kind::ALL.into_iter().find(|kind| {
        value
            .strip_prefix(kind.name())
            .and_then(|suffix| suffix.strip_prefix('-'))
            .is_some_and(|suffix| !suffix.is_empty())
    });
    if value.len() > 128
        || kind.is_none()
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(Error::Configuration(format!(
            "{SESSION_ENV} is not a valid HowTo shell session"
        )));
    }
    Ok(Some((kind.expect("checked above"), value)))
}

fn pending_file(paths: &Paths, session_hash: &str) -> PathBuf {
    paths
        .runtime_dir
        .join(format!("pending-{session_hash}.json"))
}

pub fn purge_pending(paths: &Paths) -> Result<()> {
    paths.create()?;
    for entry in fs::read_dir(&paths.runtime_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if is_pending_state_name(&name) {
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.is_file() && !metadata.file_type().is_symlink() {
                fs::remove_file(entry.path())?;
            }
        }
    }
    Ok(())
}

fn cleanup_expired_pending(paths: &Paths) -> Result<()> {
    for entry in fs::read_dir(&paths.runtime_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !is_pending_state_name(&name) {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        let expired = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Pending>(&bytes).ok())
            .is_none_or(|pending| pending_age(pending.created_at).is_none());
        if expired {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}

fn pending_age(created_at: u64) -> Option<Duration> {
    let age = now_seconds().checked_sub(created_at)?;
    let age = Duration::from_secs(age);
    (age < PENDING_TTL).then_some(age)
}

fn is_pending_state_name(name: &str) -> bool {
    name.ends_with(".json") && (name.starts_with("pending-") || name.starts_with(".pending-claim-"))
}

fn validate_command(command: &str) -> Result<()> {
    if command.is_empty() || command.len() > 4_096 || command.chars().any(char::is_control) {
        return Err(Error::InvalidResponse(
            "generated command cannot be inserted into the shell buffer".into(),
        ));
    }
    Ok(())
}

fn find_shells(kind: Kind) -> Vec<PathBuf> {
    if let Some(shell) = env::var_os("SHELL") {
        let path = PathBuf::from(shell);
        if path.file_name().and_then(|name| name.to_str()) == Some(kind.name()) && path.is_file() {
            return vec![path];
        }
    }
    env::var_os("PATH")
        .into_iter()
        .flat_map(|path| env::split_paths(&path).collect::<Vec<_>>())
        .map(|directory| directory.join(kind.name()))
        .filter(|candidate| candidate.is_file())
        .collect()
}

fn quote(kind: Kind, path: &Path) -> String {
    let value = path.as_os_str().to_string_lossy();
    match kind {
        Kind::Zsh | Kind::Bash => format!("'{}'", value.replace('\'', "'\\''")),
        Kind::Fish => format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'")),
    }
}

#[cfg(unix)]
fn file_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn file_mode(_path: &Path) -> Option<u32> {
    None
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;
    use std::sync::Mutex;

    use super::{
        detect, disable_recorded, discard_pending, enable_recorded, enable_recorded_approved,
        is_enabled_at, quote, replace_managed_block, startup_files, startup_symlink_requests,
        store_pending, take_pending, update_startup_files, Kind, BLOCK_END, BLOCK_START,
    };
    use crate::paths::Paths;

    static ENVIRONMENT: Mutex<()> = Mutex::new(());

    #[test]
    fn managed_block_is_added_replaced_and_removed() {
        let original = "export TEST=1\n";
        let first = format!("{BLOCK_START}\nfirst\n{BLOCK_END}\n");
        let second = format!("{BLOCK_START}\nsecond\n{BLOCK_END}\n");
        let added = replace_managed_block(original, Some(&first)).unwrap();
        assert!(added.contains("first"));
        let replaced = replace_managed_block(&added, Some(&second)).unwrap();
        assert!(!replaced.contains("first"));
        assert_eq!(replaced.matches(BLOCK_START).count(), 1);
        assert_eq!(replace_managed_block(&replaced, None).unwrap(), original);
        let no_newline = replace_managed_block("export TEST=1", Some(&first)).unwrap();
        assert!(no_newline.starts_with("export TEST=1\n\n# >>>"));
    }

    #[test]
    fn malformed_managed_blocks_are_rejected() {
        assert!(replace_managed_block(BLOCK_START, None).is_err());
        assert!(replace_managed_block(&format!("{BLOCK_END}\n{BLOCK_START}"), None).is_err());
    }

    #[test]
    fn adapter_paths_are_quoted_for_each_shell() {
        let path = std::path::Path::new("/tmp/HowTo user's adapter");
        assert_eq!(quote(Kind::Zsh, path), "'/tmp/HowTo user'\\''s adapter'");
        assert_eq!(quote(Kind::Bash, path), "'/tmp/HowTo user'\\''s adapter'");
        assert_eq!(quote(Kind::Fish, path), "'/tmp/HowTo user\\'s adapter'");
    }

    #[test]
    fn unsupported_login_shell_is_not_guessed() {
        let _guard = ENVIRONMENT.lock().unwrap();
        let previous = env::var_os("SHELL");
        env::set_var("SHELL", "/usr/bin/nu");
        assert!(detect().is_err());
        if let Some(previous) = previous {
            env::set_var("SHELL", previous);
        } else {
            env::remove_var("SHELL");
        }
    }

    #[cfg(unix)]
    #[test]
    fn startup_preflight_prevents_partial_changes() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.rc");
        let second = directory.path().join("second.rc");
        let target = directory.path().join("target.rc");
        fs::write(&first, "first\n").unwrap();
        fs::write(&target, "second\n").unwrap();
        symlink(&target, &second).unwrap();
        let block = format!("{BLOCK_START}\ntest\n{BLOCK_END}\n");
        assert!(update_startup_files(&[first.clone(), second], Some(&block)).is_err());
        assert_eq!(fs::read_to_string(first).unwrap(), "first\n");
    }

    #[cfg(unix)]
    #[test]
    fn approved_startup_symlink_updates_and_records_only_the_resolved_target() {
        use std::os::unix::fs::symlink;

        let _guard = ENVIRONMENT.lock().unwrap();
        let previous_home = env::var_os("HOME");
        let previous_zdotdir = env::var_os("ZDOTDIR");
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        let dotfiles = home.join("dotfiles");
        let links = home.join("links");
        fs::create_dir_all(&dotfiles).unwrap();
        fs::create_dir_all(&links).unwrap();
        let target = dotfiles.join("zshrc");
        fs::write(&target, "export USER_SETTING=1\n").unwrap();
        symlink("../dotfiles/zshrc", links.join("current")).unwrap();
        symlink("links/current", home.join(".zshrc")).unwrap();
        env::set_var("HOME", &home);
        env::set_var("ZDOTDIR", &home);

        let paths = Paths::under(directory.path().join("state"));
        let approvals = startup_symlink_requests(&paths, Kind::Zsh, &[]).unwrap();
        assert_eq!(approvals.len(), 1);
        assert_eq!(approvals[0].link_path(), home.join(".zshrc"));
        assert_eq!(approvals[0].target_path(), target.canonicalize().unwrap());
        assert!(approvals[0].managed_block().contains(BLOCK_START));

        // The ordinary API remains strict: discovering a link does not itself
        // authorize following it or create the adapter.
        assert!(enable_recorded(&paths, Kind::Zsh, &[]).is_err());
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "export USER_SETTING=1\n"
        );
        assert!(!paths.shell_dir().join("howto.zsh").exists());

        let enabled = enable_recorded_approved(&paths, Kind::Zsh, &[], &approvals).unwrap();
        assert_eq!(enabled.startup_files, vec![target.canonicalize().unwrap()]);
        assert!(fs::symlink_metadata(home.join(".zshrc"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(fs::read_to_string(&target).unwrap().contains(BLOCK_START));
        assert!(is_enabled_at(&paths, Kind::Zsh, &enabled.startup_files));

        // The exact resolved target in the receipt represents prior consent,
        // so idempotent setup does not prompt again.
        assert!(
            startup_symlink_requests(&paths, Kind::Zsh, &enabled.startup_files)
                .unwrap()
                .is_empty()
        );
        assert!(
            !enable_recorded(&paths, Kind::Zsh, &enabled.startup_files)
                .unwrap()
                .changed
        );

        // Even with a lost receipt, safe marker discovery follows the known
        // startup link and removes the managed block from the target.
        assert!(disable_recorded(&paths, Kind::Zsh, &[]).unwrap().changed);
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "export USER_SETTING=1\n"
        );
        assert!(fs::symlink_metadata(home.join(".zshrc"))
            .unwrap()
            .file_type()
            .is_symlink());

        if let Some(previous_home) = previous_home {
            env::set_var("HOME", previous_home);
        } else {
            env::remove_var("HOME");
        }
        if let Some(previous_zdotdir) = previous_zdotdir {
            env::set_var("ZDOTDIR", previous_zdotdir);
        } else {
            env::remove_var("ZDOTDIR");
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_startup_ancestor_requires_approval_and_records_canonical_target() {
        use std::os::unix::fs::symlink;

        let _guard = ENVIRONMENT.lock().unwrap();
        let previous_home = env::var_os("HOME");
        let previous_zdotdir = env::var_os("ZDOTDIR");
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        let real_config = home.join("real-config");
        fs::create_dir_all(&real_config).unwrap();
        let target = real_config.join(".zshrc");
        fs::write(&target, "export USER_SETTING=1\n").unwrap();
        let linked_config = home.join("linked-config");
        symlink("real-config", &linked_config).unwrap();
        env::set_var("HOME", &home);
        env::set_var("ZDOTDIR", &linked_config);

        let paths = Paths::under(directory.path().join("state"));
        let approvals = startup_symlink_requests(&paths, Kind::Zsh, &[]).unwrap();
        assert_eq!(approvals.len(), 1);
        assert_eq!(approvals[0].link_path(), linked_config.join(".zshrc"));
        assert_eq!(approvals[0].target_path(), target.canonicalize().unwrap());

        let error = enable_recorded(&paths, Kind::Zsh, &[]).unwrap_err();
        assert!(error.to_string().contains("explicit approval is required"));
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "export USER_SETTING=1\n"
        );

        let enabled = enable_recorded_approved(&paths, Kind::Zsh, &[], &approvals).unwrap();
        assert_eq!(enabled.startup_files, vec![target.canonicalize().unwrap()]);
        assert!(fs::read_to_string(&target).unwrap().contains(BLOCK_START));
        assert!(is_enabled_at(&paths, Kind::Zsh, &enabled.startup_files));

        if let Some(previous_home) = previous_home {
            env::set_var("HOME", previous_home);
        } else {
            env::remove_var("HOME");
        }
        if let Some(previous_zdotdir) = previous_zdotdir {
            env::set_var("ZDOTDIR", previous_zdotdir);
        } else {
            env::remove_var("ZDOTDIR");
        }
    }

    #[cfg(unix)]
    #[test]
    fn enabled_status_rejects_retargeted_removed_and_drifted_startup_paths() {
        use std::os::unix::fs::symlink;

        let _guard = ENVIRONMENT.lock().unwrap();
        let previous_home = env::var_os("HOME");
        let previous_zdotdir = env::var_os("ZDOTDIR");
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        let first = home.join("first");
        let second = home.join("second");
        let drifted = home.join("drifted");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        fs::create_dir_all(&drifted).unwrap();
        fs::write(first.join(".zshrc"), "first\n").unwrap();
        let selected = home.join("selected");
        symlink("first", &selected).unwrap();
        env::set_var("HOME", &home);
        env::set_var("ZDOTDIR", &selected);

        let paths = Paths::under(directory.path().join("state"));
        let approvals = startup_symlink_requests(&paths, Kind::Zsh, &[]).unwrap();
        let enabled = enable_recorded_approved(&paths, Kind::Zsh, &[], &approvals).unwrap();
        let configured = fs::read_to_string(first.join(".zshrc")).unwrap();
        fs::write(second.join(".zshrc"), &configured).unwrap();
        fs::write(drifted.join(".zshrc"), &configured).unwrap();
        assert!(is_enabled_at(&paths, Kind::Zsh, &enabled.startup_files));

        fs::remove_file(&selected).unwrap();
        symlink("second", &selected).unwrap();
        assert!(!is_enabled_at(&paths, Kind::Zsh, &enabled.startup_files));

        fs::remove_file(&selected).unwrap();
        fs::create_dir(&selected).unwrap();
        fs::write(selected.join(".zshrc"), &configured).unwrap();
        assert!(!is_enabled_at(&paths, Kind::Zsh, &enabled.startup_files));

        env::set_var("ZDOTDIR", &drifted);
        assert!(!is_enabled_at(&paths, Kind::Zsh, &enabled.startup_files));

        if let Some(previous_home) = previous_home {
            env::set_var("HOME", previous_home);
        } else {
            env::remove_var("HOME");
        }
        if let Some(previous_zdotdir) = previous_zdotdir {
            env::set_var("ZDOTDIR", previous_zdotdir);
        } else {
            env::remove_var("ZDOTDIR");
        }
    }

    #[cfg(unix)]
    #[test]
    fn startup_symlink_approval_is_revalidated_before_writing() {
        use std::os::unix::fs::symlink;

        let _guard = ENVIRONMENT.lock().unwrap();
        let previous_home = env::var_os("HOME");
        let previous_zdotdir = env::var_os("ZDOTDIR");
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let first = home.join("first.zsh");
        let second = home.join("second.zsh");
        fs::write(&first, "first\n").unwrap();
        fs::write(&second, "second\n").unwrap();
        let link = home.join(".zshrc");
        symlink("first.zsh", &link).unwrap();
        env::set_var("HOME", &home);
        env::set_var("ZDOTDIR", &home);

        let paths = Paths::under(directory.path().join("state"));
        let approvals = startup_symlink_requests(&paths, Kind::Zsh, &[]).unwrap();
        fs::remove_file(&link).unwrap();
        symlink("second.zsh", &link).unwrap();
        let error = enable_recorded_approved(&paths, Kind::Zsh, &[], &approvals).unwrap_err();
        assert!(error.to_string().contains("explicit approval is required"));
        assert_eq!(fs::read_to_string(first).unwrap(), "first\n");
        assert_eq!(fs::read_to_string(second).unwrap(), "second\n");
        assert!(!paths.shell_dir().join("howto.zsh").exists());

        if let Some(previous_home) = previous_home {
            env::set_var("HOME", previous_home);
        } else {
            env::remove_var("HOME");
        }
        if let Some(previous_zdotdir) = previous_zdotdir {
            env::set_var("ZDOTDIR", previous_zdotdir);
        } else {
            env::remove_var("ZDOTDIR");
        }
    }

    #[cfg(unix)]
    #[test]
    fn startup_symlink_target_must_be_safe_and_inside_home() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let _guard = ENVIRONMENT.lock().unwrap();
        let previous_home = env::var_os("HOME");
        let previous_zdotdir = env::var_os("ZDOTDIR");
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let link = home.join(".zshrc");
        let outside = directory.path().join("outside.zsh");
        fs::write(&outside, "outside\n").unwrap();
        symlink(&outside, &link).unwrap();
        env::set_var("HOME", &home);
        env::set_var("ZDOTDIR", &home);
        let paths = Paths::under(directory.path().join("state"));

        let error = startup_symlink_requests(&paths, Kind::Zsh, &[]).unwrap_err();
        assert!(error
            .to_string()
            .contains("outside the current user's home"));

        fs::remove_file(&link).unwrap();
        let writable = home.join("writable.zsh");
        fs::write(&writable, "writable\n").unwrap();
        fs::set_permissions(&writable, fs::Permissions::from_mode(0o666)).unwrap();
        symlink("writable.zsh", &link).unwrap();
        let error = startup_symlink_requests(&paths, Kind::Zsh, &[]).unwrap_err();
        assert!(error.to_string().contains("writable by another user"));

        fs::remove_file(&link).unwrap();
        symlink("missing.zsh", &link).unwrap();
        let error = startup_symlink_requests(&paths, Kind::Zsh, &[]).unwrap_err();
        assert!(error
            .to_string()
            .contains("could not resolve shell startup symlink"));

        if let Some(previous_home) = previous_home {
            env::set_var("HOME", previous_home);
        } else {
            env::remove_var("HOME");
        }
        if let Some(previous_zdotdir) = previous_zdotdir {
            env::set_var("ZDOTDIR", previous_zdotdir);
        } else {
            env::remove_var("ZDOTDIR");
        }
    }

    #[cfg(unix)]
    #[test]
    fn recorded_bash_paths_survive_login_file_precedence_changes() {
        use std::os::unix::fs::symlink;

        let _guard = ENVIRONMENT.lock().unwrap();
        let previous_home = env::var_os("HOME");
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        fs::create_dir_all(&home).unwrap();
        env::set_var("HOME", &home);

        let profile = home.join(".profile");
        let block = format!("{BLOCK_START}\nold adapter\n{BLOCK_END}\n");
        fs::write(&profile, &block).unwrap();
        fs::write(home.join(".bash_profile"), "new login file\n").unwrap();
        symlink(home.join(".bash_profile"), home.join(".bash_login")).unwrap();

        let paths = Paths::under(directory.path().join("state"));
        let result = disable_recorded(&paths, Kind::Bash, std::slice::from_ref(&profile)).unwrap();
        assert!(result.changed);
        assert!(!fs::read_to_string(&profile).unwrap().contains(BLOCK_START));
        assert_eq!(
            fs::read_to_string(home.join(".bash_profile")).unwrap(),
            "new login file\n"
        );

        if let Some(previous_home) = previous_home {
            env::set_var("HOME", previous_home);
        } else {
            env::remove_var("HOME");
        }
    }

    #[test]
    fn fish_startup_path_respects_xdg_config_home() {
        let _guard = ENVIRONMENT.lock().unwrap();
        let previous_home = env::var_os("HOME");
        let previous_xdg = env::var_os("XDG_CONFIG_HOME");
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        let config = directory.path().join("xdg-config");
        fs::create_dir_all(&home).unwrap();
        env::set_var("HOME", &home);
        env::set_var("XDG_CONFIG_HOME", &config);
        assert_eq!(
            startup_files(Kind::Fish).unwrap(),
            vec![config.join("fish/config.fish")]
        );

        env::set_var("XDG_CONFIG_HOME", "relative");
        assert!(startup_files(Kind::Fish).is_err());
        if let Some(previous_home) = previous_home {
            env::set_var("HOME", previous_home);
        } else {
            env::remove_var("HOME");
        }
        if let Some(previous_xdg) = previous_xdg {
            env::set_var("XDG_CONFIG_HOME", previous_xdg);
        } else {
            env::remove_var("XDG_CONFIG_HOME");
        }
    }

    #[cfg(unix)]
    #[test]
    fn bash_support_checks_the_configured_login_shell_first() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENVIRONMENT.lock().unwrap();
        let previous_shell = env::var_os("SHELL");
        let previous_path = env::var_os("PATH");
        let directory = tempfile::tempdir().unwrap();
        let old_dir = directory.path().join("old");
        let new_dir = directory.path().join("new");
        fs::create_dir_all(&old_dir).unwrap();
        fs::create_dir_all(&new_dir).unwrap();
        let old_bash = old_dir.join("bash");
        let new_bash = new_dir.join("bash");
        fs::write(&old_bash, "#!/bin/sh\nexit 1\n").unwrap();
        fs::write(&new_bash, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&old_bash, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&new_bash, fs::Permissions::from_mode(0o700)).unwrap();

        env::set_var("SHELL", &old_bash);
        env::set_var("PATH", &new_dir);
        assert!(super::ensure_supported(Kind::Bash).is_err());
        env::set_var("SHELL", "/bin/zsh");
        assert!(super::ensure_supported(Kind::Bash).is_ok());

        if let Some(previous_shell) = previous_shell {
            env::set_var("SHELL", previous_shell);
        } else {
            env::remove_var("SHELL");
        }
        if let Some(previous_path) = previous_path {
            env::set_var("PATH", previous_path);
        } else {
            env::remove_var("PATH");
        }
    }

    #[test]
    fn enabled_status_requires_exact_adapter_and_managed_block() {
        let _guard = ENVIRONMENT.lock().unwrap();
        let previous_home = env::var_os("HOME");
        let previous_zdotdir = env::var_os("ZDOTDIR");
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        fs::create_dir_all(&home).unwrap();
        env::set_var("HOME", &home);
        env::set_var("ZDOTDIR", &home);
        let paths = Paths::under(directory.path().join("state"));
        let enabled = enable_recorded(&paths, Kind::Zsh, &[]).unwrap();
        assert!(is_enabled_at(&paths, Kind::Zsh, &enabled.startup_files));

        fs::write(&enabled.adapter_file, "tampered\n").unwrap();
        assert!(!is_enabled_at(&paths, Kind::Zsh, &enabled.startup_files));

        if let Some(previous_home) = previous_home {
            env::set_var("HOME", previous_home);
        } else {
            env::remove_var("HOME");
        }
        if let Some(previous_zdotdir) = previous_zdotdir {
            env::set_var("ZDOTDIR", previous_zdotdir);
        } else {
            env::remove_var("ZDOTDIR");
        }
    }

    #[test]
    fn pending_commands_are_one_shot_and_session_scoped() {
        let _guard = ENVIRONMENT.lock().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::under(directory.path().to_path_buf());
        env::set_var(super::SESSION_ENV, "zsh-123");
        assert!(store_pending(&paths, "printf '%s\\n' hello").unwrap());
        env::set_var(super::SESSION_ENV, "zsh-456");
        assert_eq!(take_pending(&paths).unwrap(), None);
        env::set_var(super::SESSION_ENV, "zsh-123");
        assert_eq!(
            take_pending(&paths).unwrap().as_deref(),
            Some("printf '%s\\n' hello")
        );
        assert_eq!(take_pending(&paths).unwrap(), None);
        store_pending(&paths, "pwd").unwrap();
        assert!(discard_pending(&paths).unwrap());
        assert_eq!(take_pending(&paths).unwrap(), None);
        env::remove_var(super::SESSION_ENV);
    }

    #[test]
    fn pending_commands_reject_control_characters() {
        let _guard = ENVIRONMENT.lock().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::under(directory.path().to_path_buf());
        env::set_var(super::SESSION_ENV, "fish-123");
        assert!(store_pending(&paths, "echo ok\necho no").is_err());
        env::set_var(super::SESSION_ENV, "unknown-123");
        assert!(store_pending(&paths, "pwd").is_err());
        env::remove_var(super::SESSION_ENV);
    }

    #[test]
    fn future_dated_pending_state_is_rejected_and_removed() {
        let _guard = ENVIRONMENT.lock().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::under(directory.path().to_path_buf());
        paths.create().unwrap();
        env::set_var(super::SESSION_ENV, "zsh-future-test");
        let (session, hash) = super::session().unwrap().unwrap();
        let pending = super::Pending {
            schema: super::PENDING_SCHEMA,
            created_at: super::now_seconds() + 60,
            session,
            command: "pwd".into(),
        };
        super::write_private_atomic(
            &super::pending_file(&paths, &hash),
            &serde_json::to_vec(&pending).unwrap(),
        )
        .unwrap();
        assert_eq!(take_pending(&paths).unwrap(), None);
        assert!(!super::pending_file(&paths, &hash).exists());
        env::remove_var(super::SESSION_ENV);
    }

    #[cfg(unix)]
    #[test]
    fn pending_state_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENVIRONMENT.lock().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::under(directory.path().to_path_buf());
        env::set_var(super::SESSION_ENV, "bash-123");
        store_pending(&paths, "pwd").unwrap();
        let entry = fs::read_dir(&paths.runtime_dir)
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| entry.file_name().to_string_lossy().starts_with("pending-"))
            .unwrap();
        assert_eq!(
            fs::metadata(entry.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        env::remove_var(super::SESSION_ENV);
    }
}
