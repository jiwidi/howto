use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use fs2::FileExt;
use reqwest::blocking::{Client, Response};
use reqwest::header::{CONTENT_RANGE, RANGE};
use reqwest::StatusCode;
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::paths::Paths;

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

pub fn resolve(config: &Config, paths: &Paths) -> Result<Option<ModelLocation>> {
    if let Some(path) = env::var_os("HOWTO_MODEL").filter(|value| !value.is_empty()) {
        return validate_configured(PathBuf::from(path)).map(Some);
    }
    if let Some(path) = &config.model_path {
        return validate_configured(expand_tilde(path)).map(Some);
    }

    for candidate in packaged_candidates() {
        if is_default_model(&candidate) {
            return Ok(Some(ModelLocation::Packaged(candidate)));
        }
    }

    let managed = paths.models_dir().join(DEFAULT_MODEL.filename);
    if is_default_model(&managed) {
        return Ok(Some(ModelLocation::Managed(managed)));
    }
    Ok(None)
}

fn validate_configured(path: PathBuf) -> Result<ModelLocation> {
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
    Ok(ModelLocation::Configured(canonical))
}

fn is_plausible_model(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gguf"))
        && path.is_file()
        && fs::metadata(path).is_ok_and(|metadata| metadata.len() > 1_048_576)
        && has_gguf_magic(path)
}

fn has_gguf_magic(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut magic = [0_u8; 4];
    file.read_exact(&mut magic).is_ok() && magic == *b"GGUF"
}

fn is_default_model(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.file_type().is_file()
            && metadata.len() == DEFAULT_MODEL.size
            && is_plausible_model(path)
    })
}

fn packaged_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = env::var_os("HOWTO_PACKAGED_MODEL").filter(|value| !value.is_empty()) {
        candidates.push(PathBuf::from(path));
    }
    if let Ok(executable) = env::current_exe().and_then(fs::canonicalize) {
        if let Some(prefix) = executable.parent().and_then(Path::parent) {
            candidates.push(
                prefix
                    .join("share/howto/models")
                    .join(DEFAULT_MODEL.filename),
            );
        }
    }
    for prefix in [
        "/opt/homebrew",
        "/usr/local",
        "/home/linuxbrew/.linuxbrew",
        "/usr",
    ] {
        candidates.push(
            Path::new(prefix)
                .join("share/howto/models")
                .join(DEFAULT_MODEL.filename),
        );
    }
    candidates
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

    pub fn install<F>(&self, paths: &Paths, mut progress: F) -> Result<PathBuf>
    where
        F: FnMut(u64, u64),
    {
        paths.create()?;
        let lock = open_model_lock(&paths.model_lock_file())?;
        FileExt::lock_exclusive(&lock)?;
        let destination = paths.models_dir().join(DEFAULT_MODEL.filename);
        reject_symlink_if_present(&destination)?;
        if fs::symlink_metadata(&destination).is_ok_and(|metadata| !metadata.file_type().is_file())
        {
            return Err(Error::Model(format!(
                "refusing non-regular managed model destination {}",
                destination.display()
            )));
        }
        if destination.is_file() && fs::metadata(&destination)?.len() == DEFAULT_MODEL.size {
            if verify_sha256(&destination, DEFAULT_MODEL.sha256)? {
                return Ok(destination);
            }
            fs::remove_file(&destination)?;
        }

        let partial = destination.with_extension("gguf.part");
        reject_symlink_if_present(&partial)?;
        let mut existing = fs::metadata(&partial).map_or(0, |metadata| metadata.len());
        if existing > DEFAULT_MODEL.size {
            fs::remove_file(&partial)?;
            existing = 0;
        }
        if existing == DEFAULT_MODEL.size {
            if verify_sha256(&partial, DEFAULT_MODEL.sha256)? {
                finalize_model(&partial, &destination)?;
                return Ok(destination);
            }
            fs::remove_file(&partial)?;
            existing = 0;
        }
        let required = DEFAULT_MODEL.size.saturating_sub(existing);
        let available = fs2::available_space(paths.models_dir())?;
        let reserve = 128 * 1_024 * 1_024;
        if available < required.saturating_add(reserve) {
            return Err(Error::Model(format!(
                "not enough free disk space for the model: need {:.0} MiB plus 128 MiB working space, have {:.0} MiB",
                required / 1_048_576,
                available / 1_048_576
            )));
        }

        let mut request = self.client.get(DEFAULT_MODEL.url);
        if existing > 0 {
            request = request.header(RANGE, format!("bytes={existing}-"));
        }
        let response = request
            .send()
            .map_err(|error| Error::Network(format!("model download failed: {error}")))?;
        let (mut file, downloaded) = prepare_partial(&partial, &response, existing)?;
        copy_response(response, &mut file, downloaded, &mut progress)?;
        file.sync_all()?;

        let actual_size = fs::metadata(&partial)?.len();
        if actual_size != DEFAULT_MODEL.size {
            return Err(Error::Network(format!(
                "model download is incomplete: expected {} bytes, received {actual_size}; run again to resume",
                DEFAULT_MODEL.size
            )));
        }
        if !verify_sha256(&partial, DEFAULT_MODEL.sha256)? {
            fs::remove_file(&partial)?;
            return Err(Error::Model(
                "downloaded model failed SHA-256 verification and was removed".into(),
            ));
        }
        finalize_model(&partial, &destination)?;
        Ok(destination)
    }
}

fn finalize_model(partial: &Path, destination: &Path) -> Result<()> {
    fs::rename(partial, destination)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(destination, fs::Permissions::from_mode(0o644))?;
        if let Some(parent) = destination.parent() {
            File::open(parent)?.sync_all()?;
        }
    }
    Ok(())
}

/*
 * Keep the completed artifact immutable by name. Selecting a future model is
 * configuration, never a symlink that replaces or deletes another artifact.
 */

fn prepare_partial(path: &Path, response: &Response, existing: u64) -> Result<(File, u64)> {
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
        let valid = resume_range_matches(content_range, existing, DEFAULT_MODEL.size);
        if !valid {
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
    fs::remove_file(path).map_err(|error| {
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

fn copy_response<F>(
    response: Response,
    file: &mut File,
    already_downloaded: u64,
    progress: &mut F,
) -> Result<()>
where
    F: FnMut(u64, u64),
{
    copy_reader(
        response,
        file,
        already_downloaded,
        DEFAULT_MODEL.size,
        progress,
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

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::io::{Cursor, Seek, SeekFrom, Write};

    use super::{
        copy_reader, finalize_model, is_default_model, is_plausible_model, parse_content_range,
        partial_response_mode, reject_symlink_if_present, resume_range_matches, verify_sha256,
        PartialResponseMode, DEFAULT_MODEL,
    };
    use reqwest::StatusCode;

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
}
