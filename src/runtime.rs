use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use nix::sys::signal::{self, Signal};
use nix::unistd::{Pid, Uid};
use rand::RngCore;
use reqwest::blocking::{Client, RequestBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::paths::Paths;

const HEALTH_TIMEOUT: Duration = Duration::from_millis(800);
const REUSE_HEALTH_ATTEMPTS: usize = 3;
const SERVER_IDLE_SECONDS: &str = "900";
const SERVER_STATE_SCHEMA: u32 = 1;
// Bump whenever fixed launch flags or the managed-server environment policy changes.
const SERVER_RUNTIME_POLICY_SCHEMA: u32 = 1;
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const LOG_TAIL_READ_BYTES: u64 = 64 * 1_024;
const LOG_TAIL_DISPLAY_BYTES: usize = 8 * 1_024;

#[derive(Clone)]
pub struct Connection {
    client: Client,
    base_url: String,
    api_key: Option<String>,
    pub managed: bool,
}

impl Connection {
    pub fn get(&self, route: &str) -> RequestBuilder {
        self.authorize(self.client.get(self.url(route)))
    }

    pub fn post(&self, route: &str) -> RequestBuilder {
        self.authorize(self.client.post(self.url(route)))
    }

    fn authorize(&self, request: RequestBuilder) -> RequestBuilder {
        if let Some(key) = &self.api_key {
            request.bearer_auth(key)
        } else {
            request
        }
    }

    fn url(&self, route: &str) -> String {
        format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            route.trim_start_matches('/')
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ServerState {
    pub schema: u32,
    pub pid: u32,
    pub socket_path: PathBuf,
    pub model_path: PathBuf,
    pub server_path: PathBuf,
    pub fingerprint: String,
    pub started_at: u64,
}

pub struct Manager<'a> {
    config: &'a Config,
    paths: &'a Paths,
}

impl<'a> Manager<'a> {
    #[must_use]
    pub const fn new(config: &'a Config, paths: &'a Paths) -> Self {
        Self { config, paths }
    }

    pub fn ensure(&self, model_path: &Path) -> Result<Connection> {
        refuse_elevated()?;
        if let Some(url) = &self.config.server_url {
            return external_connection(url);
        }

        self.paths.create()?;
        validate_socket_length(&self.paths.server_socket_file())?;
        let lock = open_private_file(&self.paths.server_lock_file(), false)?;
        FileExt::lock_exclusive(&lock)?;

        let server_path = resolve_llama_server(self.config)?;
        let fingerprint = fingerprint(model_path, &server_path, self.config)?;
        if let Some(state) = self.load_state()? {
            if self.state_is_current(&state, &fingerprint) && process_matches(&state) {
                if let Ok(connection) = self.connection_for_state(&state) {
                    if is_healthy_with_retries(&connection, REUSE_HEALTH_ATTEMPTS) {
                        return Ok(connection);
                    }
                }
            }
            self.stop_state_if_owned(&state)?;
            self.clean_state_files()?;
        }

        self.start(model_path, &server_path, fingerprint)
    }

    pub fn status(&self) -> Result<Option<ServerState>> {
        self.paths.create()?;
        validate_socket_length(&self.paths.server_socket_file())?;
        let lock = open_private_file(&self.paths.server_lock_file(), false)?;
        FileExt::lock_exclusive(&lock)?;
        let Some(state) = self.load_state()? else {
            return Ok(None);
        };
        if state.schema == SERVER_STATE_SCHEMA
            && state.socket_path == self.paths.server_socket_file()
            && process_matches(&state)
            && self
                .connection_for_state(&state)
                .is_ok_and(|connection| is_healthy(&connection))
        {
            Ok(Some(state))
        } else {
            Ok(None)
        }
    }

    pub fn configured_provider_healthy(&self) -> Result<Option<bool>> {
        let Some(url) = &self.config.server_url else {
            return Ok(None);
        };
        let connection = external_connection(url)?;
        Ok(Some(["health", "v1/models"].iter().any(|route| {
            connection
                .get(route)
                .timeout(Duration::from_secs(5))
                .send()
                .is_ok_and(|response| response.status().is_success())
        })))
    }

    pub fn stop(&self) -> Result<bool> {
        refuse_elevated()?;
        self.paths.create()?;
        let lock = open_private_file(&self.paths.server_lock_file(), false)?;
        FileExt::lock_exclusive(&lock)?;
        let Some(state) = self.load_state()? else {
            self.clean_state_files()?;
            return Ok(false);
        };
        let stopped = self.stop_state_if_owned(&state)?;
        self.clean_state_files()?;
        Ok(stopped)
    }

    fn start(
        &self,
        model_path: &Path,
        server_path: &Path,
        fingerprint: String,
    ) -> Result<Connection> {
        self.clean_state_files()?;
        let api_key = generate_api_key();
        write_private(&self.paths.server_key_file(), api_key.as_bytes())?;

        rotate_log(&self.paths.server_log_file())?;
        let stdout = open_log(&self.paths.server_log_file())?;
        let stderr = stdout.try_clone()?;
        let socket = self.paths.server_socket_file();

        let mut command = Command::new(server_path);
        configure_server_environment(&mut command);
        command
            .current_dir(&self.paths.runtime_dir)
            .arg("--model")
            .arg(model_path)
            .arg("--host")
            .arg(&socket)
            .arg("--ctx-size")
            .arg(self.config.context_size.to_string())
            .arg("--threads")
            .arg(self.config.threads.to_string())
            .arg("--parallel")
            .arg("1")
            .arg("--alias")
            .arg(&self.config.model_id)
            .arg("--n-predict")
            .arg(self.config.max_tokens.to_string())
            .arg("--api-key-file")
            .arg(self.paths.server_key_file())
            .arg("--offline")
            .arg("--no-webui")
            .arg("--sleep-idle-seconds")
            .arg(SERVER_IDLE_SECONDS)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                self.clean_state_files()?;
                return Err(Error::Server(format!(
                    "could not start {}: {error}",
                    server_path.display()
                )));
            }
        };
        let state = ServerState {
            schema: SERVER_STATE_SCHEMA,
            pid: child.id(),
            socket_path: socket,
            model_path: model_path.to_path_buf(),
            server_path: server_path.to_path_buf(),
            fingerprint,
            started_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        };
        if let Err(error) = self.save_state(&state) {
            let _ = child.kill();
            let _ = child.wait();
            self.clean_state_files()?;
            return Err(error);
        }
        let connection = match self.connection_for_state(&state) {
            Ok(connection) => connection,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                self.clean_state_files()?;
                return Err(error);
            }
        };
        let deadline = Instant::now() + Duration::from_secs(self.config.startup_timeout_seconds);
        while Instant::now() < deadline {
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.clean_state_files()?;
                    return Err(Error::Server(format!(
                        "llama-server exited during startup ({status}){}",
                        log_suffix(&self.paths.server_log_file())
                    )));
                }
                Ok(None) => {}
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    self.clean_state_files()?;
                    return Err(Error::Server(format!(
                        "could not inspect llama-server during startup: {error}"
                    )));
                }
            }
            if is_healthy(&connection) {
                return Ok(connection);
            }
            thread::sleep(Duration::from_millis(150));
        }

        let _ = child.kill();
        let _ = child.wait();
        self.clean_state_files()?;
        Err(Error::Server(format!(
            "llama-server did not become ready within {} seconds{}",
            self.config.startup_timeout_seconds,
            log_suffix(&self.paths.server_log_file())
        )))
    }

    fn connection_for_state(&self, state: &ServerState) -> Result<Connection> {
        if state.socket_path != self.paths.server_socket_file() {
            return Err(Error::Server(
                "server state contains an unexpected socket path".into(),
            ));
        }
        let api_key = fs::read_to_string(self.paths.server_key_file()).map_err(|error| {
            Error::Server(format!("could not read the local server key: {error}"))
        })?;
        let client = Client::builder()
            .unix_socket(state.socket_path.clone())
            .no_proxy()
            .connect_timeout(HEALTH_TIMEOUT)
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|error| {
                Error::Server(format!("could not initialize local HTTP client: {error}"))
            })?;
        Ok(Connection {
            client,
            base_url: "http://localhost".into(),
            api_key: Some(api_key.trim().into()),
            managed: true,
        })
    }

    fn state_is_current(&self, state: &ServerState, fingerprint: &str) -> bool {
        state.schema == SERVER_STATE_SCHEMA
            && state.fingerprint == fingerprint
            && state.socket_path == self.paths.server_socket_file()
    }

    fn load_state(&self) -> Result<Option<ServerState>> {
        let path = self.paths.server_state_file();
        if !path.exists() {
            return Ok(None);
        }
        if fs::symlink_metadata(&path)?.file_type().is_symlink() {
            return Err(Error::Server(format!(
                "refusing symlinked server state {}",
                path.display()
            )));
        }
        let bytes = fs::read(&path)?;
        let state = match serde_json::from_slice(&bytes) {
            Ok(state) => state,
            Err(error) => {
                let suffix = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let quarantined =
                    path.with_extension(format!("json.corrupt-{suffix}-{}", std::process::id()));
                fs::rename(&path, &quarantined).map_err(|rename_error| {
                    Error::Server(format!(
                        "could not parse {} ({error}) or quarantine it: {rename_error}",
                        path.display()
                    ))
                })?;
                self.clean_state_files()?;
                eprintln!(
                    "Warning: quarantined malformed server state as {}.",
                    quarantined.display()
                );
                return Ok(None);
            }
        };
        Ok(Some(state))
    }

    fn save_state(&self, state: &ServerState) -> Result<()> {
        let data = serde_json::to_vec_pretty(state)?;
        let temporary = self.paths.server_state_file().with_extension("json.tmp");
        let _ = fs::remove_file(&temporary);
        write_private(&temporary, &data)?;
        fs::rename(temporary, self.paths.server_state_file())?;
        Ok(())
    }

    fn stop_state_if_owned(&self, state: &ServerState) -> Result<bool> {
        if !process_matches(state) {
            return Ok(false);
        }
        let Ok(pid) = i32::try_from(state.pid) else {
            return Ok(false);
        };
        let pid = Pid::from_raw(pid);
        match signal::kill(pid, Signal::SIGTERM) {
            Ok(()) => {}
            Err(nix::errno::Errno::ESRCH) => return Ok(true),
            Err(error) => {
                return Err(Error::Server(format!(
                    "could not stop llama-server: {error}"
                )));
            }
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if !process_is_alive(pid) {
                return Ok(true);
            }
            thread::sleep(Duration::from_millis(100));
        }
        if process_matches(state) {
            match signal::kill(pid, Signal::SIGKILL) {
                Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
                Err(error) => {
                    return Err(Error::Server(format!(
                        "could not force-stop llama-server: {error}"
                    )));
                }
            }
        }
        Ok(true)
    }

    fn clean_state_files(&self) -> Result<()> {
        for path in [
            self.paths.server_state_file(),
            self.paths.server_key_file(),
            self.paths.server_socket_file(),
        ] {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}

pub fn refuse_elevated() -> Result<()> {
    if Uid::effective() != Uid::current() || Uid::effective().is_root() {
        return Err(Error::Execution(
            "HowTo refuses to run as root or through sudo; run it as your normal user".into(),
        ));
    }
    Ok(())
}

pub fn resolve_llama_server(config: &Config) -> Result<PathBuf> {
    if let Some(path) = env::var_os("HOWTO_LLAMA_SERVER").filter(|value| !value.is_empty()) {
        return validate_executable(PathBuf::from(path));
    }
    if let Some(path) = &config.llama_server_path {
        return validate_executable(path.clone());
    }
    if let Some(path) = find_in_path("llama-server") {
        return Ok(path);
    }
    if let Some(path) = first_valid_executable(
        [
            "/opt/homebrew/bin/llama-server",
            "/usr/local/bin/llama-server",
            "/home/linuxbrew/.linuxbrew/bin/llama-server",
            "/usr/bin/llama-server",
        ]
        .into_iter()
        .map(PathBuf::from),
    ) {
        return Ok(path);
    }
    let hint = if cfg!(target_os = "macos") {
        "install it with `brew install llama.cpp`"
    } else {
        "install llama.cpp (or Linuxbrew's `llama.cpp`) and put llama-server on PATH"
    };
    Err(Error::Dependency(format!(
        "llama-server was not found; {hint}"
    )))
}

pub fn probe_llama_server(path: &Path) -> bool {
    if path.file_name().and_then(|name| name.to_str()) != Some("llama-server") {
        return false;
    }
    let mut command = Command::new(path);
    configure_server_environment(&mut command);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    let deadline = Instant::now() + VERSION_PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

fn validate_executable(path: PathBuf) -> Result<PathBuf> {
    let canonical = fs::canonicalize(&path).map_err(|error| {
        Error::Dependency(format!("could not resolve {}: {error}", path.display()))
    })?;
    if !canonical.is_file() {
        return Err(Error::Dependency(format!(
            "{} is not a file",
            canonical.display()
        )));
    }
    if canonical.file_name().and_then(|name| name.to_str()) != Some("llama-server") {
        return Err(Error::Dependency(format!(
            "{} is not named llama-server",
            canonical.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(&canonical)?.permissions().mode() & 0o111 == 0 {
            return Err(Error::Dependency(format!(
                "{} is not executable",
                canonical.display()
            )));
        }
    }
    Ok(canonical)
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    first_valid_executable(env::split_paths(&path).map(|directory| directory.join(name)))
}

fn first_valid_executable(candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    candidates
        .into_iter()
        .find_map(|candidate| validate_executable(candidate).ok())
}

fn external_connection(url: &str) -> Result<Connection> {
    if let Some(path) = url.strip_prefix("unix://") {
        if path.is_empty() {
            return Err(Error::Configuration(
                "unix server_url must include an absolute socket path".into(),
            ));
        }
        let socket = PathBuf::from(path);
        if !socket.is_absolute() {
            return Err(Error::Configuration(
                "unix server_url must use an absolute socket path".into(),
            ));
        }
        let client = Client::builder()
            .unix_socket(socket)
            .no_proxy()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|error| {
                Error::Network(format!("could not initialize HTTP client: {error}"))
            })?;
        return Ok(Connection {
            client,
            base_url: "http://localhost".into(),
            api_key: env::var("HOWTO_API_KEY").ok(),
            managed: false,
        });
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(Error::Configuration(
            "server_url must start with http://, https://, or unix://".into(),
        ));
    }
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|error| Error::Network(format!("could not initialize HTTP client: {error}")))?;
    Ok(Connection {
        client,
        base_url: url.trim_end_matches('/').into(),
        api_key: env::var("HOWTO_API_KEY").ok(),
        managed: false,
    })
}

fn is_healthy(connection: &Connection) -> bool {
    connection
        .get("health")
        .timeout(HEALTH_TIMEOUT)
        .send()
        .is_ok_and(|response| response.status().is_success())
}

fn is_healthy_with_retries(connection: &Connection, attempts: usize) -> bool {
    for attempt in 0..attempts.max(1) {
        if is_healthy(connection) {
            return true;
        }
        if attempt + 1 < attempts {
            thread::sleep(Duration::from_millis(200));
        }
    }
    false
}

fn fingerprint(model: &Path, server: &Path, config: &Config) -> Result<String> {
    let model = fs::canonicalize(model)?;
    let server = fs::canonicalize(server)?;
    let metadata = fs::metadata(&model)?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs());
    let material = format!(
        "{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
        crate::VERSION,
        SERVER_RUNTIME_POLICY_SCHEMA,
        model.display(),
        metadata.len(),
        modified,
        server.display(),
        config.threads,
        config.context_size,
        config.max_tokens,
        config.model_id
    );
    Ok(hex::encode(Sha256::digest(material.as_bytes())))
}

fn generate_api_key() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn configure_server_environment(command: &mut Command) {
    // The managed server is a security boundary. In particular, llama.cpp
    // accepts LLAMA_ARG_* environment overrides that could undo explicit
    // offline, authentication, host, UI, or prompt-logging settings.
    command.env_clear();
    for key in [
        "HOME",
        "TMPDIR",
        "TMP",
        "TEMP",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "CUDA_VISIBLE_DEVICES",
        "HIP_VISIBLE_DEVICES",
        "ROCR_VISIBLE_DEVICES",
    ] {
        if let Some(value) = env::var_os(key) {
            command.env(key, value);
        }
    }
}

fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    Ok(())
}

fn open_private_file(path: &Path, truncate: bool) -> Result<File> {
    if path.exists() && fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(Error::Server(format!(
            "refusing symlinked file {}",
            path.display()
        )));
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .truncate(truncate);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    Ok(options.open(path)?)
}

fn open_log(path: &Path) -> Result<File> {
    if path.exists() && fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(Error::Server(format!(
            "refusing symlinked log {}",
            path.display()
        )));
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    Ok(options.open(path)?)
}

fn rotate_log(path: &Path) -> Result<()> {
    if fs::metadata(path).is_ok_and(|metadata| metadata.len() > 2 * 1_024 * 1_024) {
        let previous = path.with_extension("log.1");
        let _ = fs::remove_file(&previous);
        fs::rename(path, previous)?;
    }
    Ok(())
}

fn process_matches(state: &ServerState) -> bool {
    let Ok(raw_pid) = i32::try_from(state.pid) else {
        return false;
    };
    let pid = Pid::from_raw(raw_pid);
    if !process_is_alive(pid) {
        return false;
    }
    let Some(command) = process_command(state.pid) else {
        return false;
    };
    let server_name = state
        .server_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("llama-server");
    command.contains(server_name)
        && command.contains(&state.model_path.to_string_lossy().to_string())
        && command.contains(&state.socket_path.to_string_lossy().to_string())
}

fn process_is_alive(pid: Pid) -> bool {
    signal::kill(pid, None).is_ok()
}

fn process_command(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let bytes = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        let command = bytes
            .split(|byte| *byte == 0)
            .map(|part| String::from_utf8_lossy(part))
            .collect::<Vec<_>>()
            .join(" ");
        if !command.is_empty() {
            return Some(command);
        }
    }
    let output = Command::new("ps")
        .args(["-ww", "-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().into())
}

fn validate_socket_length(path: &Path) -> Result<()> {
    let length = path.as_os_str().to_string_lossy().len();
    let maximum = if cfg!(target_os = "macos") { 103 } else { 107 };
    if length > maximum {
        return Err(Error::Configuration(format!(
            "runtime path is too long for a Unix socket ({length} > {maximum}): {}; shorten HOWTO_HOME",
            path.display()
        )));
    }
    Ok(())
}

fn log_suffix(path: &Path) -> String {
    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    let length = file.metadata().map_or(0, |metadata| metadata.len());
    let start = length.saturating_sub(LOG_TAIL_READ_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut bytes = Vec::new();
    if Read::take(&mut file, LOG_TAIL_READ_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return String::new();
    }
    if start > 0 {
        if let Some(newline) = bytes.iter().position(|byte| *byte == b'\n') {
            bytes.drain(..=newline);
        } else {
            bytes.clear();
        }
    }
    let contents = String::from_utf8_lossy(&bytes);
    let tail = contents.lines().rev().take(8).collect::<Vec<_>>();
    if tail.is_empty() {
        String::new()
    } else {
        let text = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
        format!("\nrecent server log:\n{}", terminal_safe_log_text(&text))
    }
}

fn terminal_safe_log_text(value: &str) -> String {
    let mut output = String::with_capacity(value.len().min(LOG_TAIL_DISPLAY_BYTES));
    let mut truncated = false;
    for character in value.chars() {
        let unsafe_character = character.is_control() && character != '\n'
            || matches!(
                character,
                '\u{061c}'
                    | '\u{200b}'..='\u{200f}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2060}'..='\u{206f}'
                    | '\u{feff}'
            );
        let escaped = if unsafe_character {
            format!("\\u{{{:x}}}", u32::from(character))
        } else {
            character.to_string()
        };
        if output.len() + escaped.len() > LOG_TAIL_DISPLAY_BYTES {
            truncated = true;
            break;
        }
        output.push_str(&escaped);
    }
    if truncated {
        while output.len() + '…'.len_utf8() > LOG_TAIL_DISPLAY_BYTES {
            output.pop();
        }
        output.push('…');
    }
    output
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use super::{
        configure_server_environment, first_valid_executable, generate_api_key, log_suffix,
        probe_llama_server, validate_socket_length, LOG_TAIL_DISPLAY_BYTES,
    };
    use crate::config::Config;
    use crate::paths::Paths;

    #[test]
    fn api_keys_are_random_and_long() {
        let first = generate_api_key();
        let second = generate_api_key();
        assert_eq!(first.len(), 64);
        assert_ne!(first, second);
    }

    #[test]
    fn rejects_overlong_socket_paths() {
        let path = std::path::PathBuf::from(format!("/tmp/{}/server.sock", "x".repeat(200)));
        assert!(validate_socket_length(&path).is_err());
    }

    #[test]
    fn process_command_reads_this_test() {
        if let Some(command) = super::process_command(std::process::id()) {
            assert!(!command.is_empty());
        }
        let _ = fs::metadata("/tmp").unwrap();
    }

    #[test]
    fn managed_server_environment_is_allowlisted() {
        let mut command = Command::new("llama-server");
        command.env("LLAMA_ARG_LOG_PROMPTS_DIR", "/tmp/leak");
        configure_server_environment(&mut command);
        let allowed = [
            "HOME",
            "TMPDIR",
            "TMP",
            "TEMP",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "CUDA_VISIBLE_DEVICES",
            "HIP_VISIBLE_DEVICES",
            "ROCR_VISIBLE_DEVICES",
        ];
        for (key, _) in command.get_envs() {
            assert!(allowed.contains(&key.to_string_lossy().as_ref()));
            assert!(!key.to_string_lossy().starts_with("LLAMA_"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn runtime_probe_requires_named_successful_version_command() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        assert!(!probe_llama_server(std::path::Path::new("/usr/bin/true")));

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("llama-server");
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(b"#!/bin/sh\nprintf 'version: test\\n'\n")
            .unwrap();
        drop(file);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(probe_llama_server(&path));

        fs::write(&path, b"#!/bin/sh\nexit 1\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!probe_llama_server(&path));
    }

    #[cfg(unix)]
    #[test]
    fn executable_discovery_skips_invalid_candidates() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let invalid_directory = directory.path().join("invalid");
        let valid_directory = directory.path().join("valid");
        fs::create_dir_all(&invalid_directory).unwrap();
        fs::create_dir_all(&valid_directory).unwrap();

        let invalid = invalid_directory.join("llama-server");
        fs::write(&invalid, b"not executable\n").unwrap();
        fs::set_permissions(&invalid, fs::Permissions::from_mode(0o600)).unwrap();

        let valid = valid_directory.join("llama-server");
        let mut file = fs::File::create(&valid).unwrap();
        file.write_all(b"#!/bin/sh\nexit 0\n").unwrap();
        drop(file);
        fs::set_permissions(&valid, fs::Permissions::from_mode(0o700)).unwrap();

        let resolved = first_valid_executable(vec![invalid, valid.clone()]).unwrap();
        assert_eq!(resolved, fs::canonicalize(valid).unwrap());
    }

    #[test]
    fn malformed_state_is_quarantined_and_recovered() {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::under(directory.path().to_path_buf());
        paths.create().unwrap();
        fs::write(paths.server_state_file(), b"{not-json").unwrap();
        fs::write(paths.server_key_file(), b"stale-key").unwrap();

        let config = Config::default();
        let manager = super::Manager::new(&config, &paths);
        assert!(manager.status().unwrap().is_none());
        assert!(!paths.server_state_file().exists());
        assert!(!paths.server_key_file().exists());
        assert!(fs::read_dir(&paths.runtime_dir).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("server.json.corrupt-")
        }));
    }

    #[test]
    fn runtime_log_tail_is_bounded_and_terminal_safe() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("server.log");
        fs::write(
            &path,
            format!("{}\n\u{1b}[31mboom\u{202e}\n", "x".repeat(128 * 1_024)),
        )
        .unwrap();
        let suffix = log_suffix(&path);
        assert!(suffix.len() <= LOG_TAIL_DISPLAY_BYTES + 64);
        assert!(!suffix.contains('\u{1b}'));
        assert!(!suffix.contains('\u{202e}'));
        assert!(suffix.contains("\\u{1b}"));
    }
}
