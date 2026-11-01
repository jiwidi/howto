use std::collections::HashSet;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use fs2::FileExt;
use reqwest::blocking::{Client, Response};
use reqwest::header::{CONTENT_RANGE, RANGE};
use reqwest::StatusCode;
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::model_catalog::{Artifact, ArtifactFiles, ModelEntry, ModelProfile, Release};
use crate::paths::Paths;

/// Compatibility metadata for the model shipped by HowTo 0.1.x.
///
/// New code should use [`ModelProfile::entry`] and [`ResolvedModel`]. Keeping this manifest
/// stable lets existing callers and the legacy flat model layout continue to work during the
/// catalog migration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Manifest {
    pub id: &'static str,
    pub display_name: &'static str,
    pub filename: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
    pub license: &'static str,
}

pub const DEFAULT_MODEL: Manifest = Manifest {
    id: "nl2sh-1.5b-q4-k-m",
    display_name: "nl2sh 1.5B Q4_K_M",
    filename: "nl2sh-1.5b-Q4_K_M.gguf",
    url: "https://huggingface.co/ThorOdinson246/nl2sh-1.5b-Q4_K_M/resolve/36a06980dccf66995c8544aa4e33bf060cb26299/nl2sh-1.5b-Q4_K_M.gguf",
    sha256: "6f8a17a11129a31074c944f4c2602453fafd9de43bdaeb1630a8f511ec820f71",
    size: 986_048_000,
    license: "Apache-2.0",
};

/// Compatibility representation of the base model's location.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelLocation {
    Configured(PathBuf),
    Packaged(PathBuf),
    Managed(PathBuf),
}

impl ModelLocation {
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            Self::Configured(path) | Self::Packaged(path) | Self::Managed(path) => path,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelSource {
    Configured,
    Packaged,
    Managed,
}

/// A complete model selection ready for the embedded engine.
///
/// Configured paths deliberately have no catalog metadata: `HOWTO_MODEL` and `model_path` are
/// expert overrides and may point at any compatible GGUF. Catalog models always carry their
/// immutable artifact metadata, including an optional LoRA file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedModel {
    pub source: ModelSource,
    pub profile: Option<ModelProfile>,
    pub artifact_id: Option<&'static str>,
    pub model_path: PathBuf,
    pub lora_path: Option<PathBuf>,
    pub artifacts: Option<ArtifactFiles>,
}

impl ResolvedModel {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.model_path
    }

    #[must_use]
    pub fn location(&self) -> ModelLocation {
        match self.source {
            ModelSource::Configured => ModelLocation::Configured(self.model_path.clone()),
            ModelSource::Packaged => ModelLocation::Packaged(self.model_path.clone()),
            ModelSource::Managed => ModelLocation::Managed(self.model_path.clone()),
        }
    }

    #[must_use]
    pub fn entry(&self) -> Option<&'static ModelEntry> {
        self.profile.map(ModelProfile::entry)
    }
}

/// Resolve the configured model bundle without downloading anything.
///
/// Precedence is intentionally explicit: a configured remote provider uses no local model;
/// otherwise `HOWTO_MODEL` overrides `model_path`, and both override the catalog profile. The
/// package and managed directories are only searched for the selected catalog entry.
pub fn resolve_model(config: &Config, paths: &Paths) -> Result<Option<ResolvedModel>> {
    if config.server_url.is_some() {
        return Ok(None);
    }
    if let Some(path) = env::var_os("HOWTO_MODEL").filter(|value| !value.is_empty()) {
        return validate_configured(PathBuf::from(path)).map(Some);
    }
    if let Some(path) = &config.model_path {
        return validate_configured(expand_tilde(path)).map(Some);
    }

    resolve_profile(config.model_profile, paths)
}

/// Resolve a catalog profile from a package or HowTo's managed model directory.
pub fn resolve_profile(profile: ModelProfile, paths: &Paths) -> Result<Option<ResolvedModel>> {
    let entry = profile.entry();
    let files = published_files(entry)?;

    if let Some(path) = env::var_os("HOWTO_PACKAGED_MODEL").filter(|value| !value.is_empty()) {
        let base = PathBuf::from(path);
        return bundle_at_base(entry, files, ModelSource::Packaged, base)
            .map(Some)
            .map_err(|error| {
                Error::Model(format!(
                    "HOWTO_PACKAGED_MODEL does not provide a complete {} bundle: {error}",
                    entry.display_name
                ))
            });
    }

    for base in packaged_candidates(entry) {
        if !base.exists() {
            continue;
        }
        return bundle_at_base(entry, files, ModelSource::Packaged, base)
            .map(Some)
            .map_err(|error| {
                Error::Model(format!(
                    "the packaged {} bundle is incomplete or invalid: {error}",
                    entry.display_name
                ))
            });
    }

    resolve_managed_profile(entry, files, paths)
}

/// Compatibility wrapper returning only the base model location.
pub fn resolve(config: &Config, paths: &Paths) -> Result<Option<ModelLocation>> {
    resolve_model(config, paths).map(|model| model.map(|resolved| resolved.location()))
}

/// Verify every catalog artifact in a resolved bundle by exact byte count and SHA-256.
///
/// `None` means the model is an arbitrary configured override and therefore has no pinned hash.
pub fn verify_resolved(model: &ResolvedModel) -> Result<Option<bool>> {
    let Some(files) = model.artifacts else {
        return Ok(None);
    };
    if !verify_artifact(&model.model_path, files.base)? {
        return Ok(Some(false));
    }
    match (files.lora, model.lora_path.as_deref()) {
        (None, None) => Ok(Some(true)),
        (Some(artifact), Some(path)) => verify_artifact(path, artifact).map(Some),
        _ => Ok(Some(false)),
    }
}

fn published_files(entry: &ModelEntry) -> Result<ArtifactFiles> {
    match entry.release {
        Release::Published(files) => {
            validate_catalog_bundle(entry.artifact_id, files)?;
            Ok(files)
        }
        Release::Unpublished { reason } => Err(Error::Model(format!(
            "the {} model profile is not available for installation yet: {reason}",
            entry.profile
        ))),
    }
}

fn validate_configured(path: PathBuf) -> Result<ResolvedModel> {
    let canonical = fs::canonicalize(&path).map_err(|error| {
        Error::Model(format!(
            "could not resolve configured model {}: {error}",
            path.display()
        ))
    })?;
    if !is_plausible_model(&canonical) {
        return Err(Error::Model(format!(
            "configured model is missing or not a regular GGUF file: {}",
            canonical.display()
        )));
    }
    Ok(ResolvedModel {
        source: ModelSource::Configured,
        profile: None,
        artifact_id: None,
        model_path: canonical,
        lora_path: None,
        artifacts: None,
    })
}

fn bundle_if_complete(
    entry: &'static ModelEntry,
    files: ArtifactFiles,
    source: ModelSource,
    base: PathBuf,
) -> Result<Option<ResolvedModel>> {
    if !base.exists() {
        return Ok(None);
    }
    if !is_artifact_candidate(&base, files.base) {
        return Ok(None);
    }
    let lora_path = files.lora.map(|artifact| {
        base.parent()
            .unwrap_or_else(|| Path::new(""))
            .join(artifact.filename)
    });
    if let (Some(artifact), Some(path)) = (files.lora, lora_path.as_deref()) {
        if !is_artifact_candidate(path, artifact) {
            return Ok(None);
        }
    }
    Ok(Some(resolved_catalog_model(
        entry, files, source, base, lora_path,
    )))
}

fn bundle_at_base(
    entry: &'static ModelEntry,
    files: ArtifactFiles,
    source: ModelSource,
    base: PathBuf,
) -> Result<ResolvedModel> {
    if !is_artifact_candidate(&base, files.base) {
        return Err(Error::Model(format!(
            "{} must be a regular {}-byte GGUF file",
            base.display(),
            files.base.size_bytes
        )));
    }
    let lora_path = files.lora.map(|artifact| {
        base.parent()
            .unwrap_or_else(|| Path::new(""))
            .join(artifact.filename)
    });
    if let (Some(artifact), Some(path)) = (files.lora, lora_path.as_deref()) {
        if !is_artifact_candidate(path, artifact) {
            return Err(Error::Model(format!(
                "{} must be a regular {}-byte GGUF file",
                path.display(),
                artifact.size_bytes
            )));
        }
    }
    Ok(resolved_catalog_model(
        entry, files, source, base, lora_path,
    ))
}

fn resolved_catalog_model(
    entry: &'static ModelEntry,
    files: ArtifactFiles,
    source: ModelSource,
    model_path: PathBuf,
    lora_path: Option<PathBuf>,
) -> ResolvedModel {
    ResolvedModel {
        source,
        profile: Some(entry.profile),
        artifact_id: Some(entry.artifact_id),
        model_path,
        lora_path,
        artifacts: Some(files),
    }
}

fn resolve_managed_profile(
    entry: &'static ModelEntry,
    files: ArtifactFiles,
    paths: &Paths,
) -> Result<Option<ResolvedModel>> {
    let managed_base = managed_bundle_dir(paths, entry, files)?.join(files.base.filename);
    if let Some(bundle) = bundle_if_complete(entry, files, ModelSource::Managed, managed_base)? {
        return Ok(Some(bundle));
    }

    // HowTo 0.1.x put the legacy model directly in the models directory. It remains readable so
    // upgrades never force a second 940 MiB download.
    if entry.profile == ModelProfile::Legacy {
        let flat = paths.models_dir().join(files.base.filename);
        if let Some(bundle) = bundle_if_complete(entry, files, ModelSource::Managed, flat)? {
            return Ok(Some(bundle));
        }
    }

    Ok(None)
}

fn is_plausible_model(path: &Path) -> bool {
    is_regular_file_without_symlink(path)
        && path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("gguf"))
        && fs::metadata(path).is_ok_and(|metadata| metadata.len() > 1_048_576)
        && has_gguf_magic(path)
}

fn is_artifact_candidate(path: &Path, artifact: Artifact) -> bool {
    is_regular_file_without_symlink(path)
        && fs::metadata(path).is_ok_and(|metadata| metadata.len() == artifact.size_bytes)
        && has_gguf_magic(path)
}

fn is_regular_file_without_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
}

fn has_gguf_magic(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut magic = [0_u8; 4];
    file.read_exact(&mut magic).is_ok() && magic == *b"GGUF"
}

#[cfg(test)]
fn is_default_model(path: &Path) -> bool {
    is_artifact_candidate(path, ModelProfile::Legacy.entry().artifacts().unwrap().base)
}

fn packaged_candidates(entry: &ModelEntry) -> Vec<PathBuf> {
    let files = entry
        .artifacts()
        .expect("packaged candidates require a published model entry");
    let mut roots = Vec::new();
    if let Ok(executable) = env::current_exe().and_then(fs::canonicalize) {
        if let Some(prefix) = executable.parent().and_then(Path::parent) {
            roots.push(prefix.to_path_buf());
        }
    }
    roots.extend(
        [
            "/opt/homebrew",
            "/usr/local",
            "/home/linuxbrew/.linuxbrew",
            "/usr",
        ]
        .into_iter()
        .map(PathBuf::from),
    );

    let mut candidates = Vec::with_capacity(roots.len() * 2);
    let bundle_id = bundle_storage_id(entry, files)
        .expect("published catalog entries have validated artifact identities");
    for root in roots {
        let models = root.join("share/howto/models");
        candidates.push(models.join(&bundle_id).join(files.base.filename));
        // Flat package layouts are retained for the legacy artifact only.
        if entry.profile == ModelProfile::Legacy {
            candidates.push(models.join(files.base.filename));
        }
    }
    candidates
}

fn managed_bundle_dir(paths: &Paths, entry: &ModelEntry, files: ArtifactFiles) -> Result<PathBuf> {
    Ok(paths.models_dir().join(bundle_storage_id(entry, files)?))
}

fn bundle_storage_id(entry: &ModelEntry, files: ArtifactFiles) -> Result<String> {
    validate_catalog_bundle(entry.artifact_id, files)?;
    let mut identity = format!("{}-{}", entry.artifact_id, &files.base.sha256[..12]);
    if let Some(lora) = files.lora {
        identity.push('-');
        identity.push_str(&lora.sha256[..12]);
    }
    Ok(identity)
}

fn expand_tilde(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if text == "~" {
        return dirs::home_dir().unwrap_or_else(|| path.to_path_buf());
    }
    if let Some(suffix) = text.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(suffix);
        }
    }
    path.to_path_buf()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InstallProgress {
    pub artifact: Artifact,
    pub artifact_downloaded: u64,
    pub artifact_total: u64,
    pub bundle_downloaded: u64,
    pub bundle_total: u64,
}

pub struct Installer {
    client: Client,
}

impl Installer {
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(24 * 60 * 60))
            .user_agent(format!("howto/{}", crate::VERSION))
            .build()
            .map_err(|error| {
                Error::Network(format!("could not initialize HTTP client: {error}"))
            })?;
        Ok(Self { client })
    }

    /// Install a catalog profile as a complete, immutable bundle.
    ///
    /// Downloads are resumed inside a hidden staging directory. The profile directory only
    /// becomes visible after every base/LoRA artifact has passed exact size and SHA-256 checks.
    pub fn install_profile<F>(
        &self,
        paths: &Paths,
        profile: ModelProfile,
        mut progress: F,
    ) -> Result<ResolvedModel>
    where
        F: FnMut(InstallProgress),
    {
        let entry = profile.entry();
        let files = published_files(entry)?;
        self.install_bundle(paths, entry, files, &mut progress)
    }

    /// Compatibility wrapper which installs the legacy default model.
    pub fn install<F>(&self, paths: &Paths, mut progress: F) -> Result<PathBuf>
    where
        F: FnMut(u64, u64),
    {
        let model = self.install_profile(paths, ModelProfile::Legacy, |state| {
            progress(state.bundle_downloaded, state.bundle_total);
        })?;
        Ok(model.model_path)
    }

    fn install_bundle<F>(
        &self,
        paths: &Paths,
        entry: &'static ModelEntry,
        files: ArtifactFiles,
        progress: &mut F,
    ) -> Result<ResolvedModel>
    where
        F: FnMut(InstallProgress),
    {
        paths.create()?;
        let lock = open_model_lock(&paths.model_lock_file())?;
        FileExt::lock_exclusive(&lock)?;

        let models_dir = paths.models_dir();
        let bundle_id = bundle_storage_id(entry, files)?;
        let destination = managed_bundle_dir(paths, entry, files)?;
        if let Some(model) = exact_bundle_at(entry, files, ModelSource::Managed, &destination)? {
            return Ok(model);
        }

        let staging = models_dir.join(format!(".install-{bundle_id}"));
        create_safe_staging_directory(&staging)?;
        let artifacts = bundle_artifacts(files);
        let bundle_total = artifacts.iter().try_fold(0_u64, |total, artifact| {
            total.checked_add(artifact.size_bytes).ok_or_else(|| {
                Error::Model("model bundle byte count exceeds the supported range".into())
            })
        })?;
        let remaining_bundle_bytes = artifacts.iter().try_fold(0_u64, |total, artifact| {
            let destination = safe_artifact_path(&staging, artifact.filename)?;
            if verify_artifact(&destination, *artifact)? {
                return Ok(total);
            }
            let partial = safe_artifact_path(&staging, &format!("{}.part", artifact.filename))?;
            reject_symlink_if_present(&partial)?;
            let partial_bytes = fs::metadata(&partial)
                .ok()
                .filter(|metadata| metadata.is_file())
                .map_or(0, |metadata| metadata.len().min(artifact.size_bytes));
            total
                .checked_add(artifact.size_bytes.saturating_sub(partial_bytes))
                .ok_or_else(|| Error::Model("model bundle byte count overflowed".into()))
        })?;
        ensure_available_space(paths, remaining_bundle_bytes)?;
        let mut completed_before = 0_u64;
        for artifact in artifacts {
            self.install_artifact(
                paths,
                &staging,
                artifact,
                completed_before,
                bundle_total,
                progress,
            )?;
            completed_before = completed_before
                .checked_add(artifact.size_bytes)
                .ok_or_else(|| Error::Model("model bundle byte count overflowed".into()))?;
        }

        ensure_only_bundle_files(&staging, files)?;
        sync_directory(&staging)?;
        publish_staging_directory(&models_dir, &staging, &destination, &bundle_id)?;

        exact_bundle_at(entry, files, ModelSource::Managed, &destination)?.ok_or_else(|| {
            Error::Model(format!(
                "installed {} bundle disappeared before it could be opened",
                entry.display_name
            ))
        })
    }

    fn install_artifact<F>(
        &self,
        paths: &Paths,
        staging: &Path,
        artifact: Artifact,
        completed_before: u64,
        bundle_total: u64,
        progress: &mut F,
    ) -> Result<()>
    where
        F: FnMut(InstallProgress),
    {
        validate_artifact(artifact)?;
        let destination = safe_artifact_path(staging, artifact.filename)?;
        reject_symlink_if_present(&destination)?;
        if verify_artifact(&destination, artifact)? {
            progress(InstallProgress {
                artifact,
                artifact_downloaded: artifact.size_bytes,
                artifact_total: artifact.size_bytes,
                bundle_downloaded: completed_before.saturating_add(artifact.size_bytes),
                bundle_total,
            });
            return Ok(());
        }
        remove_regular_file_if_present(&destination)?;

        let partial = safe_artifact_path(staging, &format!("{}.part", artifact.filename))?;
        reject_symlink_if_present(&partial)?;
        let mut existing = fs::metadata(&partial).map_or(0, |metadata| metadata.len());
        if existing > artifact.size_bytes {
            remove_regular_file_if_present(&partial)?;
            existing = 0;
        }
        if existing == artifact.size_bytes {
            if verify_sha256(&partial, artifact.sha256)? {
                finalize_model(&partial, &destination)?;
                progress(InstallProgress {
                    artifact,
                    artifact_downloaded: artifact.size_bytes,
                    artifact_total: artifact.size_bytes,
                    bundle_downloaded: completed_before.saturating_add(artifact.size_bytes),
                    bundle_total,
                });
                return Ok(());
            }
            remove_regular_file_if_present(&partial)?;
            existing = 0;
        }

        let required = artifact.size_bytes.saturating_sub(existing);
        ensure_available_space(paths, required)?;
        let mut request = self.client.get(artifact.url);
        if existing > 0 {
            request = request.header(RANGE, format!("bytes={existing}-"));
        }
        let response = request.send().map_err(|error| {
            Error::Network(format!("download of {} failed: {error}", artifact.filename))
        })?;
        let (mut file, downloaded) =
            prepare_partial(&partial, &response, existing, artifact.size_bytes)?;
        copy_response(
            response,
            &mut file,
            downloaded,
            artifact,
            completed_before,
            bundle_total,
            progress,
        )?;
        file.sync_all()?;

        let actual_size = fs::metadata(&partial)?.len();
        if actual_size != artifact.size_bytes {
            return Err(Error::Network(format!(
                "download of {} is incomplete: expected {} bytes, received {actual_size}; run again to resume",
                artifact.filename, artifact.size_bytes
            )));
        }
        if !verify_sha256(&partial, artifact.sha256)? {
            remove_regular_file_if_present(&partial)?;
            return Err(Error::Model(format!(
                "downloaded {} failed SHA-256 verification and was removed",
                artifact.filename
            )));
        }
        finalize_model(&partial, &destination)
    }
}

fn validate_catalog_bundle(artifact_id: &str, files: ArtifactFiles) -> Result<()> {
    validate_artifact_id(artifact_id)?;
    validate_artifact(files.base)?;
    if let Some(lora) = files.lora {
        validate_artifact(lora)?;
        if lora.filename == files.base.filename {
            return Err(Error::Model(
                "model base and LoRA artifacts must have different filenames".into(),
            ));
        }
    }
    Ok(())
}

fn validate_artifact_id(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        || !value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
    {
        return Err(Error::Model(format!(
            "unsafe model artifact id {value:?}; expected 1-128 ASCII letters, digits, '.', '_', or '-' and an alphanumeric first character"
        )));
    }
    Ok(())
}

fn validate_artifact(artifact: Artifact) -> Result<()> {
    safe_relative_filename(artifact.filename)?;
    if artifact.size_bytes <= 1_048_576 {
        return Err(Error::Model(format!(
            "artifact {} has an implausible byte count",
            artifact.filename
        )));
    }
    if artifact.sha256.len() != 64
        || !artifact
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(Error::Model(format!(
            "artifact {} does not have a canonical SHA-256 digest",
            artifact.filename
        )));
    }
    let url = reqwest::Url::parse(artifact.url).map_err(|error| {
        Error::Model(format!(
            "artifact {} has an invalid download URL: {error}",
            artifact.filename
        ))
    })?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::Model(format!(
            "artifact {} must use an HTTPS URL without credentials or a fragment",
            artifact.filename
        )));
    }
    Ok(())
}

fn safe_relative_filename(filename: &str) -> Result<()> {
    let path = Path::new(filename);
    let mut components = path.components();
    let one_normal_component =
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none();
    if filename.is_empty()
        || filename.len() > 255
        || !one_normal_component
        || !filename
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        || !filename
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        || !filename.ends_with(".gguf")
    {
        return Err(Error::Model(format!(
            "unsafe model artifact filename {filename:?}"
        )));
    }
    Ok(())
}

fn safe_artifact_path(directory: &Path, filename: &str) -> Result<PathBuf> {
    safe_relative_filename(filename.strip_suffix(".part").unwrap_or(filename))?;
    Ok(directory.join(filename))
}

fn bundle_artifacts(files: ArtifactFiles) -> Vec<Artifact> {
    [Some(files.base), files.lora]
        .into_iter()
        .flatten()
        .collect()
}

fn create_safe_staging_directory(path: &Path) -> Result<()> {
    reject_symlink_if_present(path)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_dir() => {
            return Err(Error::Model(format!(
                "refusing non-directory model staging path {}",
                path.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path)?;
        }
        Err(error) => return Err(error.into()),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn ensure_only_bundle_files(directory: &Path, files: ArtifactFiles) -> Result<()> {
    let expected = bundle_artifacts(files)
        .into_iter()
        .map(|artifact| artifact.filename)
        .collect::<HashSet<_>>();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(Error::Model(format!(
                "model staging directory contains a non-UTF-8 entry: {}",
                entry.path().display()
            )));
        };
        if !expected.contains(name) || !entry.file_type()?.is_file() {
            return Err(Error::Model(format!(
                "model staging directory contains unexpected entry {}; remove it and retry",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

fn exact_bundle_at(
    entry: &'static ModelEntry,
    files: ArtifactFiles,
    source: ModelSource,
    directory: &Path,
) -> Result<Option<ResolvedModel>> {
    if !directory.exists() {
        return Ok(None);
    }
    let metadata = fs::symlink_metadata(directory)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Ok(None);
    }
    let base = safe_artifact_path(directory, files.base.filename)?;
    if !verify_artifact(&base, files.base)? {
        return Ok(None);
    }
    let lora_path = files
        .lora
        .map(|artifact| safe_artifact_path(directory, artifact.filename))
        .transpose()?;
    if let (Some(artifact), Some(path)) = (files.lora, lora_path.as_deref()) {
        if !verify_artifact(path, artifact)? {
            return Ok(None);
        }
    }
    Ok(Some(resolved_catalog_model(
        entry, files, source, base, lora_path,
    )))
}

fn publish_staging_directory(
    models_dir: &Path,
    staging: &Path,
    destination: &Path,
    artifact_id: &str,
) -> Result<()> {
    reject_symlink_if_present(destination)?;
    let replaced = if destination.exists() {
        let replaced = models_dir.join(format!(
            ".replaced-{artifact_id}-{}-{:016x}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::rename(destination, &replaced)?;
        Some(replaced)
    } else {
        None
    };

    if let Err(error) = fs::rename(staging, destination) {
        if let Some(replaced) = &replaced {
            let _ = fs::rename(replaced, destination);
        }
        return Err(error.into());
    }
    sync_directory(models_dir)?;
    if let Some(replaced) = replaced {
        remove_file_or_directory(&replaced)?;
        sync_directory(models_dir)?;
    }
    Ok(())
}

fn remove_file_or_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(Error::Model(format!(
            "refusing to remove symlink {}",
            path.display()
        )));
    }
    if metadata.is_dir() {
        fs::remove_dir_all(path)?;
    } else if metadata.file_type().is_file() {
        fs::remove_file(path)?;
    } else {
        return Err(Error::Model(format!(
            "refusing to remove non-regular model path {}",
            path.display()
        )));
    }
    Ok(())
}

fn remove_regular_file_if_present(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            fs::remove_file(path)?;
            Ok(())
        }
        Ok(_) => Err(Error::Model(format!(
            "refusing to replace non-regular model file {}",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn ensure_available_space(paths: &Paths, required: u64) -> Result<()> {
    let available = fs2::available_space(paths.models_dir())?;
    let reserve = 128 * 1_024 * 1_024;
    if available < required.saturating_add(reserve) {
        return Err(Error::Model(format!(
            "not enough free disk space for the model: need {:.0} MiB plus 128 MiB working space, have {:.0} MiB",
            required / 1_048_576,
            available / 1_048_576
        )));
    }
    Ok(())
}

fn verify_artifact(path: &Path, artifact: Artifact) -> Result<bool> {
    if !is_regular_file_without_symlink(path)
        || fs::metadata(path).is_ok_and(|metadata| metadata.len() != artifact.size_bytes)
    {
        return Ok(false);
    }
    verify_sha256(path, artifact.sha256)
}

fn finalize_model(partial: &Path, destination: &Path) -> Result<()> {
    reject_symlink_if_present(destination)?;
    fs::rename(partial, destination)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(destination, fs::Permissions::from_mode(0o644))?;
    }
    if let Some(parent) = destination.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

fn prepare_partial(
    path: &Path,
    response: &Response,
    existing: u64,
    expected_size: u64,
) -> Result<(File, u64)> {
    let status = response.status();
    let append = match partial_response_mode(status, existing) {
        PartialResponseMode::Append => true,
        PartialResponseMode::Replace => false,
        PartialResponseMode::DiscardAndRestart => {
            discard_partial(path)?;
            return Err(Error::Network(format!(
                "model host rejected the resume offset with HTTP {status}; the partial download was removed, run again to restart"
            )));
        }
        PartialResponseMode::Reject => {
            return Err(Error::Network(format!("model host returned HTTP {status}")));
        }
    };
    if append {
        let content_range = response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if !resume_range_matches(content_range, existing, expected_size) {
            discard_partial(path)?;
            return Err(Error::Network(format!(
                "model host returned an invalid resume range and the partial download was removed: {content_range}; run again to restart"
            )));
        }
    }
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    if append {
        file.seek(SeekFrom::End(0))?;
        Ok((file, existing))
    } else {
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        Ok((file, 0))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PartialResponseMode {
    Append,
    Replace,
    DiscardAndRestart,
    Reject,
}

fn partial_response_mode(status: StatusCode, existing: u64) -> PartialResponseMode {
    if existing > 0 && status == StatusCode::RANGE_NOT_SATISFIABLE {
        PartialResponseMode::DiscardAndRestart
    } else if !status.is_success() || (existing == 0 && status == StatusCode::PARTIAL_CONTENT) {
        PartialResponseMode::Reject
    } else if existing > 0 && status == StatusCode::PARTIAL_CONTENT {
        PartialResponseMode::Append
    } else {
        PartialResponseMode::Replace
    }
}

fn discard_partial(path: &Path) -> Result<()> {
    remove_regular_file_if_present(path).map_err(|error| {
        Error::Network(format!(
            "could not remove unusable partial model download {}: {error}",
            path.display()
        ))
    })
}

fn parse_content_range(value: &str) -> Option<(u64, u64, u64)> {
    let range = value.strip_prefix("bytes ")?;
    let (bounds, total) = range.split_once('/')?;
    let (start, end) = bounds.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?, total.parse().ok()?))
}

fn resume_range_matches(value: &str, expected_start: u64, expected_total: u64) -> bool {
    parse_content_range(value).is_some_and(|(start, end, total)| {
        start == expected_start && end >= start && end < total && total == expected_total
    })
}

#[allow(clippy::too_many_arguments)]
fn copy_response<F>(
    response: Response,
    file: &mut File,
    already_downloaded: u64,
    artifact: Artifact,
    completed_before: u64,
    bundle_total: u64,
    progress: &mut F,
) -> Result<()>
where
    F: FnMut(InstallProgress),
{
    copy_reader(
        response,
        file,
        already_downloaded,
        artifact.size_bytes,
        &mut |artifact_downloaded, artifact_total| {
            progress(InstallProgress {
                artifact,
                artifact_downloaded,
                artifact_total,
                bundle_downloaded: completed_before.saturating_add(artifact_downloaded),
                bundle_total,
            });
        },
    )
}

fn copy_reader<R, F>(
    mut reader: R,
    file: &mut File,
    already_downloaded: u64,
    maximum_size: u64,
    progress: &mut F,
) -> Result<()>
where
    R: Read,
    F: FnMut(u64, u64),
{
    let mut downloaded = already_downloaded;
    progress(downloaded, maximum_size);
    let mut buffer = vec![0_u8; 256 * 1_024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| Error::Network(format!("model download was interrupted: {error}")))?;
        if read == 0 {
            break;
        }
        let next = downloaded
            .checked_add(read as u64)
            .ok_or_else(|| Error::Network("model download byte count overflowed".into()))?;
        if next > maximum_size {
            return Err(Error::Network(
                "model host returned more data than the pinned artifact size".into(),
            ));
        }
        file.write_all(&buffer[..read])?;
        downloaded = next;
        progress(downloaded, maximum_size);
    }
    Ok(())
}

pub fn verify_sha256(path: &Path, expected: &str) -> Result<bool> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()).eq_ignore_ascii_case(expected))
}

fn reject_symlink_if_present(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(Error::Model(format!(
            "refusing to write through symlink {}",
            path.display()
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn open_model_lock(path: &Path) -> Result<File> {
    reject_symlink_if_present(path)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    Ok(options.open(path)?)
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::io::{Cursor, Seek, SeekFrom, Write};

    use super::{
        bundle_if_complete, bundle_storage_id, copy_reader, finalize_model, is_default_model,
        is_plausible_model, managed_bundle_dir, parse_content_range, partial_response_mode,
        reject_symlink_if_present, resolve_managed_profile, resume_range_matches,
        safe_artifact_path, validate_artifact_id, verify_resolved, verify_sha256, Installer,
        ModelSource, PartialResponseMode, ResolvedModel, DEFAULT_MODEL,
    };
    use crate::model_catalog::{Artifact, ArtifactFiles, ModelProfile};
    use crate::paths::Paths;
    use reqwest::StatusCode;
    use sha2::{Digest, Sha256};

    #[test]
    fn legacy_manifest_matches_the_catalog() {
        let entry = ModelProfile::Legacy.entry();
        let artifact = entry.artifacts().unwrap().base;
        assert_eq!(DEFAULT_MODEL.id, entry.artifact_id);
        assert_eq!(DEFAULT_MODEL.filename, artifact.filename);
        assert_eq!(DEFAULT_MODEL.url, artifact.url);
        assert_eq!(DEFAULT_MODEL.sha256, artifact.sha256);
        assert_eq!(DEFAULT_MODEL.size, artifact.size_bytes);
        assert_eq!(DEFAULT_MODEL.license, entry.license.base_spdx);
    }

    #[test]
    fn unpublished_profiles_fail_before_any_download() {
        if ModelProfile::Balanced.entry().is_installable() {
            // A generated publication catalog is exercised by the importer
            // tests. Never let this unit test contact a real artifact URL.
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::under(directory.path().join("howto"));
        let error = Installer::new()
            .unwrap()
            .install_profile(&paths, ModelProfile::Balanced, |_| {})
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("not available for installation yet"));
        assert!(!paths.models_dir().exists());
    }

    #[test]
    fn legacy_flat_layout_remains_resolvable() {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::under(directory.path().join("howto"));
        fs::create_dir_all(paths.models_dir()).unwrap();
        let entry = ModelProfile::Legacy.entry();
        let files = entry.artifacts().unwrap();
        let flat = paths.models_dir().join(files.base.filename);
        write_sized_gguf(&flat, files.base.size_bytes);

        let resolved = resolve_managed_profile(entry, files, &paths)
            .unwrap()
            .expect("legacy flat model");
        assert_eq!(resolved.source, ModelSource::Managed);
        assert_eq!(resolved.profile, Some(ModelProfile::Legacy));
        assert_eq!(resolved.model_path, flat);
        assert!(resolved.lora_path.is_none());
    }

    #[test]
    fn verifies_sha256() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample");
        fs::write(&path, b"HowTo\n").unwrap();
        assert!(verify_sha256(
            &path,
            "f72f73203613756e2bbfdeb65d5ab9956d4408104adf720a6678660ce203322c"
        )
        .unwrap());
        assert!(!verify_sha256(&path, &"0".repeat(64)).unwrap());
    }

    #[test]
    fn configured_models_need_gguf_magic() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("model.gguf");
        let mut file = File::create(&path).unwrap();
        file.set_len(1_048_577).unwrap();
        assert!(!is_plausible_model(&path));

        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(b"GGUF").unwrap();
        file.sync_all().unwrap();
        assert!(is_plausible_model(&path));
    }

    #[test]
    fn bundle_paths_are_profile_scoped_and_reject_traversal() {
        let paths = Paths::under(PathBuf::from("/tmp/howto-model-layout"));
        let entry = ModelProfile::Legacy.entry();
        let files = entry.artifacts().unwrap();
        assert_eq!(
            managed_bundle_dir(&paths, entry, files).unwrap(),
            PathBuf::from(format!(
                "/tmp/howto-model-layout/models/{}-{}",
                entry.artifact_id,
                &files.base.sha256[..12]
            ))
        );
        for invalid in ["", ".hidden", "../escape", "a/b", "-leading", "bad space"] {
            assert!(validate_artifact_id(invalid).is_err(), "{invalid}");
        }
        assert!(safe_artifact_path(Path::new("/safe"), "../bad.gguf").is_err());
        assert!(safe_artifact_path(Path::new("/safe"), "bad name.gguf").is_err());
        assert!(safe_artifact_path(Path::new("/safe"), "bad\\name.gguf").is_err());
        assert!(safe_artifact_path(Path::new("/safe"), "model.gguf.part").is_ok());
    }

    #[test]
    fn managed_storage_identity_changes_with_catalog_hash_not_only_size() {
        let entry = ModelProfile::Legacy.entry();
        let current = entry.artifacts().unwrap();
        let replacement = ArtifactFiles {
            base: Artifact {
                sha256: "0f8a17a11129a31074c944f4c2602453fafd9de43bdaeb1630a8f511ec820f71",
                ..current.base
            },
            lora: current.lora,
        };
        assert_eq!(current.base.size_bytes, replacement.base.size_bytes);
        assert_ne!(
            bundle_storage_id(entry, current).unwrap(),
            bundle_storage_id(entry, replacement).unwrap()
        );
    }

    #[test]
    fn incomplete_lora_bundle_is_not_resolved() {
        let directory = tempfile::tempdir().unwrap();
        let base_path = directory.path().join("base.gguf");
        write_sized_gguf(&base_path, 1_048_577);
        let files = ArtifactFiles {
            base: artifact("base.gguf", 1_048_577, &base_path),
            lora: Some(Artifact {
                filename: "adapter.gguf",
                url: "https://example.invalid/adapter.gguf",
                sha256: "0",
                size_bytes: 1_048_577,
            }),
        };
        assert!(bundle_if_complete(
            ModelProfile::Legacy.entry(),
            files,
            ModelSource::Managed,
            base_path
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn exact_bundle_verification_checks_every_artifact() {
        let directory = tempfile::tempdir().unwrap();
        let base_path = directory.path().join("base.gguf");
        let lora_path = directory.path().join("adapter.gguf");
        write_sized_gguf(&base_path, 1_048_577);
        write_sized_gguf(&lora_path, 1_048_578);
        let files = ArtifactFiles {
            base: artifact("base.gguf", 1_048_577, &base_path),
            lora: Some(artifact("adapter.gguf", 1_048_578, &lora_path)),
        };
        let resolved = ResolvedModel {
            source: ModelSource::Managed,
            profile: Some(ModelProfile::Legacy),
            artifact_id: Some("test"),
            model_path: base_path,
            lora_path: Some(lora_path.clone()),
            artifacts: Some(files),
        };
        assert_eq!(verify_resolved(&resolved).unwrap(), Some(true));
        let mut file = OpenOptions::new().write(true).open(lora_path).unwrap();
        file.seek(SeekFrom::Start(4)).unwrap();
        file.write_all(b"tampered").unwrap();
        assert_eq!(verify_resolved(&resolved).unwrap(), Some(false));
    }

    #[cfg(unix)]
    #[test]
    fn default_model_candidates_must_not_be_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let actual = directory.path().join("actual.gguf");
        let link = directory.path().join("linked.gguf");
        let mut file = File::create(&actual).unwrap();
        file.set_len(DEFAULT_MODEL.size).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(b"GGUF").unwrap();
        drop(file);
        symlink(&actual, &link).unwrap();

        assert!(is_default_model(&actual));
        assert!(!is_default_model(&link));
    }

    #[cfg(unix)]
    #[test]
    fn managed_model_destination_symlinks_are_rejected() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let actual = directory.path().join("actual.gguf");
        let destination = directory.path().join(DEFAULT_MODEL.filename);
        fs::write(&actual, b"GGUF fixture").unwrap();
        symlink(&actual, &destination).unwrap();

        let error = reject_symlink_if_present(&destination).unwrap_err();
        assert!(error
            .to_string()
            .contains("refusing to write through symlink"));
    }

    #[test]
    fn finalizing_a_model_renames_the_artifact() {
        let directory = tempfile::tempdir().unwrap();
        let partial = directory.path().join("model.gguf.part");
        let destination = directory.path().join("model.gguf");
        fs::write(&partial, b"GGUF fixture").unwrap();

        finalize_model(&partial, &destination).unwrap();

        assert!(!partial.exists());
        assert_eq!(fs::read(&destination).unwrap(), b"GGUF fixture");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(destination).unwrap().permissions().mode() & 0o777,
                0o644
            );
        }
    }

    #[test]
    fn content_ranges_are_parsed_strictly() {
        assert_eq!(parse_content_range("bytes 10-19/100"), Some((10, 19, 100)));
        assert!(resume_range_matches("bytes 10-19/100", 10, 100));
        assert!(!resume_range_matches("bytes 9-19/100", 10, 100));
        assert!(!resume_range_matches("bytes 10-9/100", 10, 100));
        assert!(!resume_range_matches("bytes 10-19/99", 10, 100));
        assert!(!resume_range_matches("", 10, 100));
        for malformed in [
            "bytes 10-/100",
            "bytes 10-19/*",
            "bytes 10-19/100 trailing",
            "items 10-19/100",
            "bytes 10-19-20/100",
        ] {
            assert_eq!(parse_content_range(malformed), None, "{malformed}");
        }
    }

    #[test]
    fn oversized_chunk_is_rejected_before_it_is_written() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("partial");
        let mut file = File::create(&path).unwrap();
        file.write_all(b"1234").unwrap();
        let mut progress = |_, _| {};
        assert!(copy_reader(Cursor::new(b"56"), &mut file, 4, 5, &mut progress).is_err());
        drop(file);
        assert_eq!(fs::read(path).unwrap(), b"1234");
    }

    #[test]
    fn resume_statuses_fail_closed_and_recover_from_416() {
        assert_eq!(
            partial_response_mode(StatusCode::PARTIAL_CONTENT, 100),
            PartialResponseMode::Append
        );
        assert_eq!(
            partial_response_mode(StatusCode::OK, 100),
            PartialResponseMode::Replace
        );
        assert_eq!(
            partial_response_mode(StatusCode::RANGE_NOT_SATISFIABLE, 100),
            PartialResponseMode::DiscardAndRestart
        );
        assert_eq!(
            partial_response_mode(StatusCode::PARTIAL_CONTENT, 0),
            PartialResponseMode::Reject
        );
    }

    fn write_sized_gguf(path: &Path, size: u64) {
        let mut file = File::create(path).unwrap();
        file.set_len(size).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(b"GGUF").unwrap();
    }

    fn artifact(filename: &'static str, size: u64, path: &Path) -> Artifact {
        let digest = hex::encode(Sha256::digest(fs::read(path).unwrap()));
        Artifact {
            filename,
            url: "https://example.invalid/model.gguf",
            sha256: Box::leak(digest.into_boxed_str()),
            size_bytes: size,
        }
    }

    use std::fs::OpenOptions;
    use std::path::{Path, PathBuf};
}
