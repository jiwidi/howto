//! Lifecycle and private IPC for the resident embedded inference engine.
//!
//! The CLI remains short lived, while a hidden invocation of the same binary
//! owns the native model. Requests use a private Unix socket and a random key;
//! no HTTP listener or separately-installed executable is involved.

use std::env;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use nix::errno::Errno;
use nix::sys::signal::{self, Signal};
use nix::unistd::{Pid, Uid};
use rand::Rng;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{Accelerator, Config};
use crate::embedded::{
    ChatMessage as EmbeddedMessage, ChatRole as EmbeddedRole, EmbeddedEngine, EmbeddedError,
    EngineOptions, GenerationFinish, GenerationOptions, GpuLayers, LoraAdapterOptions,
};
use crate::engine_protocol::{
    self, ChatRole, EngineCommand, EngineFailure, EngineFailureKind, EngineRequest, EngineResponse,
    FinishReason, GenerateRequest, GenerateResponse, ENGINE_PROTOCOL_SCHEMA,
};
use crate::error::{terminal_safe_message, Error, Result};
use crate::paths::Paths;

pub const ENGINE_WORKER_ARGUMENT: &str = "__howto-engine-worker-v1";
pub const ENGINE_IDLE_TIMEOUT: Duration = Duration::from_secs(900);

const ENGINE_STATE_SCHEMA: u32 = 1;
const ENGINE_LAUNCH_SCHEMA: u32 = 1;
// Bump when launch flags, prompt rendering, or the environment allowlist changes.
const ENGINE_RUNTIME_POLICY_SCHEMA: u32 = 2;
const HEALTH_TIMEOUT: Duration = Duration::from_millis(800);
const REUSE_HEALTH_ATTEMPTS: usize = 3;
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(100);
const PROTOCOL_READ_TIMEOUT: Duration = Duration::from_secs(5);
const PROTOCOL_WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_GENERATION_DURATION: Duration = Duration::from_secs(300);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
const LOG_TAIL_READ_BYTES: u64 = 64 * 1_024;
const LOG_TAIL_DISPLAY_BYTES: usize = 8 * 1_024;
const MAX_LOG_BYTES: u64 = 2 * 1_024 * 1_024;

/// Prompt rendering selected by the model catalog.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptFormat {
    /// Apply the default chat template embedded in the GGUF.
    ModelDefault,
    /// Standard Qwen ChatML without a thinking prefill.
    QwenChatMl,
    /// Qwen 3.5 ChatML with an already-closed empty thinking block.
    Qwen35NoThinking,
}

impl PromptFormat {
    /// Render catalog-owned prompt formats for qualification tools. `None`
    /// means the model's GGUF chat template must be applied by the engine.
    #[must_use]
    pub fn render_explicit(self, messages: &[EmbeddedMessage]) -> Option<String> {
        match self {
            Self::ModelDefault => None,
            Self::QwenChatMl => Some(render_qwen_chatml(messages)),
            Self::Qwen35NoThinking => Some(render_qwen35_no_thinking(messages)),
        }
    }
}

/// Serializable accelerator policy carried into the worker manifest.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "layers")]
pub enum GpuLayerPolicy {
    Auto,
    Cpu,
    Count(u32),
}

/// Everything that identifies a loaded engine and must survive self-reexec.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EngineSpec {
    pub model_path: PathBuf,
    pub lora_adapter: Option<PathBuf>,
    pub lora_scale: f32,
    pub context_size: u32,
    pub batch_size: u32,
    pub threads: usize,
    pub gpu_layers: GpuLayerPolicy,
    pub use_mmap: bool,
    pub prompt_format: PromptFormat,
    /// Included in the fingerprint even though requests carry their own limit.
    pub max_tokens: usize,
    /// Included so catalog/model alias changes cannot reuse an old worker.
    pub model_id: String,
}

impl EngineSpec {
    pub fn from_config(model_path: impl Into<PathBuf>, config: &Config) -> Result<Self> {
        let context_size = u32::try_from(config.context_size).map_err(|_| {
            Error::Configuration("context_size is too large for the embedded engine".into())
        })?;
        Ok(Self {
            model_path: model_path.into(),
            lora_adapter: None,
            lora_scale: 1.0,
            context_size,
            batch_size: context_size.min(512),
            threads: config.threads,
            gpu_layers: match config.accelerator {
                Accelerator::Auto => GpuLayerPolicy::Auto,
                Accelerator::Cpu => GpuLayerPolicy::Cpu,
            },
            use_mmap: true,
            prompt_format: PromptFormat::ModelDefault,
            max_tokens: config.max_tokens,
            model_id: config.model_id.clone(),
        })
    }

    pub fn validate(&self) -> Result<()> {
        if !self.model_path.is_absolute() {
            return Err(Error::Configuration(
                "embedded engine model_path must be absolute".into(),
            ));
        }
        if self
            .lora_adapter
            .as_ref()
            .is_some_and(|path| !path.is_absolute())
        {
            return Err(Error::Configuration(
                "embedded engine LoRA path must be absolute".into(),
            ));
        }
        if !self.lora_scale.is_finite() || self.lora_scale <= 0.0 {
            return Err(Error::Configuration(
                "embedded engine LoRA scale must be finite and greater than zero".into(),
            ));
        }
        if self.context_size == 0 || self.batch_size == 0 {
            return Err(Error::Configuration(
                "embedded engine context_size and batch_size must be greater than zero".into(),
            ));
        }
        if self.threads == 0 || i32::try_from(self.threads).is_err() {
            return Err(Error::Configuration(
                "embedded engine threads must be between 1 and i32::MAX".into(),
            ));
        }
        if self.max_tokens == 0 || self.max_tokens >= self.context_size as usize {
            return Err(Error::Configuration(
                "embedded engine max_tokens must be positive and smaller than context_size".into(),
            ));
        }
        if self.model_id.is_empty() || self.model_id.len() > 128 {
            return Err(Error::Configuration(
                "embedded engine model_id must be between 1 and 128 bytes".into(),
            ));
        }
        Ok(())
    }

    fn normalized(&self) -> Result<Self> {
        self.validate()?;
        let mut normalized = self.clone();
        normalized.model_path = canonical_regular_file(&self.model_path, "model")?;
        normalized.lora_adapter = self
            .lora_adapter
            .as_deref()
            .map(|path| canonical_regular_file(path, "LoRA adapter"))
            .transpose()?;
        Ok(normalized)
    }

    fn engine_options(&self) -> EngineOptions {
        EngineOptions {
            context_size: self.context_size,
            batch_size: self.batch_size,
            threads: self.threads,
            gpu_layers: match self.gpu_layers {
                GpuLayerPolicy::Auto => GpuLayers::Auto,
                GpuLayerPolicy::Cpu => GpuLayers::Cpu,
                GpuLayerPolicy::Count(layers) => GpuLayers::Count(layers),
            },
            use_mmap: self.use_mmap,
            lora_adapter: self.lora_adapter.as_ref().map(|path| LoraAdapterOptions {
                path: path.clone(),
                scale: self.lora_scale,
            }),
        }
    }
}

/// Non-secret state used for status, reuse, and process identity checks.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EngineState {
    pub schema: u32,
    pub pid: u32,
    pub socket_path: PathBuf,
    pub launch_path: PathBuf,
    pub executable_path: PathBuf,
    pub model_path: PathBuf,
    pub lora_adapter: Option<PathBuf>,
    pub prompt_format: PromptFormat,
    pub fingerprint: String,
    pub identity: String,
    pub started_at: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct EngineLaunch {
    schema: u32,
    runtime_policy_schema: u32,
    executable_path: PathBuf,
    socket_path: PathBuf,
    key_path: PathBuf,
    fingerprint: String,
    identity: String,
    idle_timeout_seconds: u64,
    spec: EngineSpec,
}

/// An authenticated handle to one resident engine.
#[derive(Clone)]
pub struct Connection {
    socket_path: PathBuf,
    authorization: String,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Connection")
            .field("socket_path", &self.socket_path)
            .finish_non_exhaustive()
    }
}

impl Connection {
    pub fn request(&self, command: EngineCommand, timeout: Duration) -> Result<EngineResponse> {
        if timeout.is_zero() {
            return Err(Error::Configuration(
                "embedded engine request timeout must be greater than zero".into(),
            ));
        }
        validate_private_socket(&self.socket_path)?;
        let mut stream = UnixStream::connect(&self.socket_path).map_err(|error| {
            Error::Server(format!("could not connect to the embedded engine: {error}"))
        })?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout.min(PROTOCOL_WRITE_TIMEOUT)))?;
        let request = EngineRequest {
            schema: ENGINE_PROTOCOL_SCHEMA,
            authorization: self.authorization.clone(),
            command,
        };
        request.validate()?;
        engine_protocol::write_frame(&mut stream, &request)?;
        let response: EngineResponse = engine_protocol::read_frame(&mut stream)?;
        response.validate()?;
        Ok(response)
    }

    pub fn generate(
        &self,
        request: GenerateRequest,
        timeout: Duration,
    ) -> Result<GenerateResponse> {
        match self.request(EngineCommand::Generate(request), timeout)? {
            EngineResponse::Generated(generation) => Ok(generation),
            EngineResponse::Failed(failure) => Err(failure_error(failure)),
            other => Err(Error::InvalidResponse(format!(
                "embedded engine returned an unexpected response: {other:?}"
            ))),
        }
    }

    fn is_healthy(&self, timeout: Duration) -> bool {
        matches!(
            self.request(EngineCommand::Health, timeout),
            Ok(EngineResponse::Healthy)
        )
    }
}

pub struct Manager<'a> {
    config: &'a Config,
    paths: &'a Paths,
}

#[derive(Clone, Copy)]
enum LockAcquisition {
    Blocking,
    NonBlocking,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExistingWorkerAction {
    Reuse,
    Restart,
    Busy,
}

fn existing_worker_action(
    current: bool,
    healthy: bool,
    socket_can_queue: bool,
) -> ExistingWorkerAction {
    if current && (healthy || socket_can_queue) {
        ExistingWorkerAction::Reuse
    } else if !current && !healthy && socket_can_queue {
        // A sequential worker cannot answer Health while it is generating.
        // A profile/configuration switch must wait rather than kill that
        // in-flight request.
        ExistingWorkerAction::Busy
    } else {
        ExistingWorkerAction::Restart
    }
}

impl<'a> Manager<'a> {
    #[must_use]
    pub const fn new(config: &'a Config, paths: &'a Paths) -> Self {
        Self { config, paths }
    }

    pub fn ensure(&self, spec: &EngineSpec) -> Result<Connection> {
        self.ensure_with_options(spec, LockAcquisition::Blocking, None)?
            .ok_or_else(|| Error::Server("could not acquire the embedded engine lock".into()))
    }

    /// Optional callers never wait on the lifecycle lock and never exceed the
    /// supplied cold-start budget. `None` means they should silently skip.
    pub fn ensure_bounded(
        &self,
        spec: &EngineSpec,
        maximum_startup_timeout: Duration,
    ) -> Result<Option<Connection>> {
        if maximum_startup_timeout.is_zero() {
            return Err(Error::Configuration(
                "bounded embedded engine startup timeout must be greater than zero".into(),
            ));
        }
        self.ensure_with_options(
            spec,
            LockAcquisition::NonBlocking,
            Some(maximum_startup_timeout),
        )
    }

    fn ensure_with_options(
        &self,
        spec: &EngineSpec,
        acquisition: LockAcquisition,
        maximum_startup_timeout: Option<Duration>,
    ) -> Result<Option<Connection>> {
        crate::runtime::refuse_elevated()?;
        let configured_timeout = Duration::from_secs(self.config.startup_timeout_seconds);
        let deadline =
            maximum_startup_timeout.map(|maximum| Instant::now() + configured_timeout.min(maximum));

        self.paths.create()?;
        validate_socket_length(&self.paths.engine_socket_file())?;
        let Some(_lock) = acquire_lock(&self.paths.engine_lock_file(), acquisition)? else {
            return Ok(None);
        };

        let executable = current_executable()?;
        let spec = spec.normalized()?;
        let fingerprint = fingerprint(&spec, &executable)?;
        if let Some(state) = self.load_state()? {
            let owned = process_matches(&state);
            if owned {
                let current = self.state_is_current(&state, &spec, &executable, &fingerprint)?;
                if let Ok(connection) = self.connection_for_state(&state) {
                    let socket_can_queue = validate_private_socket(&state.socket_path).is_ok();
                    if current && socket_can_queue {
                        // The worker is deliberately sequential. Its private
                        // listener remains queueable while a generation is in
                        // progress even though a Health request cannot be
                        // answered until that generation finishes.
                        return Ok(Some(connection));
                    }
                    let healthy = if let Some(deadline) = deadline {
                        remaining_until(deadline).is_some_and(|remaining| {
                            connection.is_healthy(remaining.min(HEALTH_TIMEOUT))
                        })
                    } else {
                        healthy_with_retries(&connection, REUSE_HEALTH_ATTEMPTS)
                    };
                    match existing_worker_action(current, healthy, socket_can_queue) {
                        ExistingWorkerAction::Reuse => return Ok(Some(connection)),
                        ExistingWorkerAction::Busy if deadline.is_some() => return Ok(None),
                        ExistingWorkerAction::Busy => {
                            return Err(Error::Server(
                                "the embedded engine is finishing another request with different model settings; retry shortly or run `howto server stop`"
                                    .into(),
                            ));
                        }
                        ExistingWorkerAction::Restart => {}
                    }
                }
            }

            // Advice is best effort and must not interrupt a worker another
            // foreground query may be loading or using.
            if deadline.is_some() && owned {
                return Ok(None);
            }
            self.stop_state_if_owned(&state)?;
            self.clean_state_files()?;
        }

        let startup_timeout = match deadline {
            Some(deadline) => match remaining_until(deadline) {
                Some(remaining) => remaining,
                None => return Ok(None),
            },
            None => configured_timeout,
        };
        self.start(
            spec,
            executable,
            fingerprint,
            startup_timeout,
            deadline.is_some(),
        )
    }

    pub fn status(&self) -> Result<Option<EngineState>> {
        self.paths.create()?;
        validate_socket_length(&self.paths.engine_socket_file())?;
        let lock = open_private_file(&self.paths.engine_lock_file(), false)?;
        FileExt::lock_exclusive(&lock)?;
        let Some(state) = self.load_state()? else {
            return Ok(None);
        };
        if state.schema == ENGINE_STATE_SCHEMA
            && state.socket_path == self.paths.engine_socket_file()
            && state.launch_path == self.paths.engine_launch_file()
            && process_matches(&state)
            && self.connection_for_state(&state).is_ok_and(|connection| {
                validate_private_socket(&state.socket_path).is_ok()
                    || connection.is_healthy(HEALTH_TIMEOUT)
            })
        {
            Ok(Some(state))
        } else {
            Ok(None)
        }
    }

    pub fn stop(&self) -> Result<bool> {
        crate::runtime::refuse_elevated()?;
        self.paths.create()?;
        let lock = open_private_file(&self.paths.engine_lock_file(), false)?;
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
        spec: EngineSpec,
        executable: PathBuf,
        fingerprint: String,
        startup_timeout: Duration,
        optional: bool,
    ) -> Result<Option<Connection>> {
        self.clean_state_files()?;
        let authorization = random_hex_key();
        let identity = random_hex_key();
        write_private(&self.paths.engine_key_file(), authorization.as_bytes())?;

        let launch = EngineLaunch {
            schema: ENGINE_LAUNCH_SCHEMA,
            runtime_policy_schema: ENGINE_RUNTIME_POLICY_SCHEMA,
            executable_path: executable.clone(),
            socket_path: self.paths.engine_socket_file(),
            key_path: self.paths.engine_key_file(),
            fingerprint: fingerprint.clone(),
            identity: identity.clone(),
            idle_timeout_seconds: ENGINE_IDLE_TIMEOUT.as_secs(),
            spec: spec.clone(),
        };
        write_private_json(&self.paths.engine_launch_file(), &launch)?;

        rotate_log(&self.paths.engine_log_file())?;
        let stdout = open_log(&self.paths.engine_log_file())?;
        let stderr = stdout.try_clone()?;
        let mut command = Command::new(&executable);
        configure_worker_environment(&mut command);
        command
            .current_dir(&self.paths.runtime_dir)
            .arg(ENGINE_WORKER_ARGUMENT)
            .arg(self.paths.engine_launch_file())
            .arg(&identity)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .process_group(0);

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                self.clean_state_files()?;
                return Err(Error::Server(format!(
                    "could not start the embedded engine: {error}"
                )));
            }
        };
        let state = EngineState {
            schema: ENGINE_STATE_SCHEMA,
            pid: child.id(),
            socket_path: self.paths.engine_socket_file(),
            launch_path: self.paths.engine_launch_file(),
            executable_path: executable,
            model_path: spec.model_path,
            lora_adapter: spec.lora_adapter,
            prompt_format: spec.prompt_format,
            fingerprint,
            identity,
            started_at: unix_seconds(),
        };
        if let Err(error) = write_private_json(&self.paths.engine_state_file(), &state) {
            terminate_child(&mut child);
            self.clean_state_files()?;
            return Err(error);
        }
        let connection = match self.connection_for_state(&state) {
            Ok(connection) => connection,
            Err(error) => {
                terminate_child(&mut child);
                self.clean_state_files()?;
                return Err(error);
            }
        };

        let deadline = Instant::now() + startup_timeout;
        while let Some(remaining) = remaining_until(deadline) {
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.clean_state_files()?;
                    return Err(Error::Server(format!(
                        "embedded engine exited during startup ({status}){}",
                        log_suffix(&self.paths.engine_log_file())
                    )));
                }
                Ok(None) => {}
                Err(error) => {
                    terminate_child(&mut child);
                    self.clean_state_files()?;
                    return Err(Error::Server(format!(
                        "could not inspect the embedded engine during startup: {error}"
                    )));
                }
            }
            if connection.is_healthy(remaining.min(HEALTH_TIMEOUT)) {
                return Ok(Some(connection));
            }
            thread::sleep(
                remaining_until(deadline)
                    .unwrap_or_default()
                    .min(STARTUP_POLL_INTERVAL),
            );
        }

        terminate_child(&mut child);
        self.clean_state_files()?;
        if optional {
            return Ok(None);
        }
        Err(Error::Server(format!(
            "embedded engine did not become ready within {} seconds{}",
            duration_seconds_rounded_up(startup_timeout),
            log_suffix(&self.paths.engine_log_file())
        )))
    }

    fn state_is_current(
        &self,
        state: &EngineState,
        spec: &EngineSpec,
        executable: &Path,
        fingerprint: &str,
    ) -> Result<bool> {
        if state.schema != ENGINE_STATE_SCHEMA
            || state.socket_path != self.paths.engine_socket_file()
            || state.launch_path != self.paths.engine_launch_file()
            || state.executable_path != executable
            || state.model_path != spec.model_path
            || state.lora_adapter != spec.lora_adapter
            || state.prompt_format != spec.prompt_format
            || state.fingerprint != fingerprint
            || !valid_hex_key(&state.identity)
        {
            return Ok(false);
        }
        let launch = match load_private_json::<EngineLaunch>(&state.launch_path) {
            Ok(launch) => launch,
            Err(_) => return Ok(false),
        };
        Ok(launch.schema == ENGINE_LAUNCH_SCHEMA
            && launch.runtime_policy_schema == ENGINE_RUNTIME_POLICY_SCHEMA
            && launch.executable_path == state.executable_path
            && launch.socket_path == state.socket_path
            && launch.key_path == self.paths.engine_key_file()
            && launch.fingerprint == state.fingerprint
            && launch.identity == state.identity
            && launch.idle_timeout_seconds == ENGINE_IDLE_TIMEOUT.as_secs()
            && launch.spec == *spec)
    }

    fn connection_for_state(&self, state: &EngineState) -> Result<Connection> {
        if state.socket_path != self.paths.engine_socket_file() {
            return Err(Error::Server(
                "embedded engine state contains an unexpected socket path".into(),
            ));
        }
        let authorization = read_private_key(&self.paths.engine_key_file())?;
        Ok(Connection {
            socket_path: state.socket_path.clone(),
            authorization,
        })
    }

    fn load_state(&self) -> Result<Option<EngineState>> {
        let path = self.paths.engine_state_file();
        if !path.exists() {
            return Ok(None);
        }
        ensure_private_regular_file(&path)?;
        let bytes = fs::read(&path)?;
        let state: EngineState = match serde_json::from_slice(&bytes) {
            Ok(state) => state,
            Err(error) => {
                let quarantined = quarantine_file(&path)?;
                self.clean_state_files()?;
                eprintln!(
                    "Warning: quarantined malformed embedded engine state as {} ({error}).",
                    quarantined.display()
                );
                return Ok(None);
            }
        };
        if state.schema > ENGINE_STATE_SCHEMA {
            return Err(Error::Server(format!(
                "embedded engine state schema {} is newer than this HowTo supports ({ENGINE_STATE_SCHEMA})",
                state.schema
            )));
        }
        if state.schema != ENGINE_STATE_SCHEMA
            || state.pid <= 1
            || !state.socket_path.is_absolute()
            || !state.launch_path.is_absolute()
            || !state.executable_path.is_absolute()
            || !state.model_path.is_absolute()
            || state
                .lora_adapter
                .as_ref()
                .is_some_and(|path| !path.is_absolute())
            || !valid_hex_key(&state.fingerprint)
            || !valid_hex_key(&state.identity)
        {
            let quarantined = quarantine_file(&path)?;
            self.clean_state_files()?;
            eprintln!(
                "Warning: quarantined invalid embedded engine state as {}.",
                quarantined.display()
            );
            return Ok(None);
        }
        Ok(Some(state))
    }

    fn stop_state_if_owned(&self, state: &EngineState) -> Result<bool> {
        if !process_matches(state) {
            return Ok(false);
        }
        if let Ok(connection) = self.connection_for_state(state) {
            let _ = connection.request(EngineCommand::Shutdown, HEALTH_TIMEOUT);
        }
        let deadline = Instant::now() + STOP_TIMEOUT;
        while Instant::now() < deadline {
            if !process_matches(state) {
                return Ok(true);
            }
            thread::sleep(Duration::from_millis(100));
        }
        signal_owned(state, Signal::SIGTERM)?;
        let deadline = Instant::now() + STOP_TIMEOUT;
        while Instant::now() < deadline {
            if !process_matches(state) {
                return Ok(true);
            }
            thread::sleep(Duration::from_millis(100));
        }
        if process_matches(state) {
            signal_owned(state, Signal::SIGKILL)?;
        }
        Ok(true)
    }

    fn clean_state_files(&self) -> Result<()> {
        for path in [
            self.paths.engine_state_file(),
            self.paths.engine_key_file(),
            self.paths.engine_socket_file(),
            self.paths.engine_launch_file(),
        ] {
            remove_runtime_file(&path)?;
        }
        Ok(())
    }
}

/// Intercept the private same-binary worker invocation before normal CLI parsing.
pub fn run_worker_from_env() -> Option<Result<i32>> {
    let mut arguments = env::args_os().skip(1);
    if arguments.next().as_deref() != Some(OsStr::new(ENGINE_WORKER_ARGUMENT)) {
        return None;
    }
    let launch_path = arguments.next();
    let identity = arguments.next();
    if arguments.next().is_some() || launch_path.is_none() || identity.is_none() {
        return Some(Err(Error::Usage(
            "invalid private embedded engine invocation".into(),
        )));
    }
    let identity = match identity.expect("checked above").into_string() {
        Ok(identity) => identity,
        Err(_) => {
            return Some(Err(Error::Usage(
                "invalid private embedded engine identity".into(),
            )));
        }
    };
    let launch_path = PathBuf::from(launch_path.expect("checked above"));
    Some(run_worker(&launch_path, &identity).map(|()| 0))
}

/// Run the sequential resident worker described by a private launch manifest.
pub fn run_worker(launch_path: &Path, identity: &str) -> Result<()> {
    crate::runtime::refuse_elevated()?;
    if !launch_path.is_absolute() || !valid_hex_key(identity) {
        return Err(Error::Server(
            "invalid embedded engine worker identity".into(),
        ));
    }
    let launch: EngineLaunch = load_private_json(launch_path)?;
    validate_launch(launch_path, identity, &launch)?;
    let authorization = read_private_key(&launch.key_path)?;

    // Initialize llama.cpp before opening the listener. Keeping the IPC file
    // descriptor out of native backend initialization also means a model-load
    // failure can never leave a connectable socket behind.
    let engine = EmbeddedEngine::load(&launch.spec.model_path, launch.spec.engine_options())
        .map_err(|error| Error::Model(format!("could not load the embedded model: {error}")))?;

    remove_runtime_file(&launch.socket_path)?;
    let listener = UnixListener::bind(&launch.socket_path).map_err(|error| {
        Error::Server(format!(
            "could not bind the embedded engine socket: {error}"
        ))
    })?;
    fs::set_permissions(&launch.socket_path, fs::Permissions::from_mode(0o600))?;
    validate_private_socket(&launch.socket_path)?;
    let _socket_guard = SocketGuard(launch.socket_path.clone());

    listener.set_nonblocking(true).map_err(|error| {
        Error::Server(format!(
            "could not configure the embedded engine socket: {error}"
        ))
    })?;
    let mut last_activity = Instant::now();
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_read_timeout(Some(PROTOCOL_READ_TIMEOUT))?;
                stream.set_write_timeout(Some(PROTOCOL_WRITE_TIMEOUT))?;
                match serve_connection(&engine, &launch, &authorization, &mut stream) {
                    Ok(WorkerAction::Continue) => {}
                    Ok(WorkerAction::Stop) => return Ok(()),
                    Err(error) => eprintln!(
                        "embedded engine rejected a local connection: {}",
                        terminal_safe_message(&error.to_string())
                    ),
                }
                last_activity = Instant::now();
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if last_activity.elapsed() >= ENGINE_IDLE_TIMEOUT {
                    return Ok(());
                }
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
            Err(error) => {
                return Err(Error::Server(format!(
                    "embedded engine socket failed: {error}"
                )));
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkerAction {
    Continue,
    Stop,
}

fn serve_connection(
    engine: &EmbeddedEngine,
    launch: &EngineLaunch,
    authorization: &str,
    stream: &mut UnixStream,
) -> Result<WorkerAction> {
    let request: EngineRequest = engine_protocol::read_frame(stream)?;
    if let Err(error) = request.validate() {
        write_failure(
            stream,
            EngineFailureKind::InvalidRequest,
            &error.to_string(),
        )?;
        return Ok(WorkerAction::Continue);
    }
    if !constant_time_eq(request.authorization.as_bytes(), authorization.as_bytes()) {
        write_failure(
            stream,
            EngineFailureKind::InvalidRequest,
            "authorization failed",
        )?;
        return Ok(WorkerAction::Continue);
    }
    match request.command {
        EngineCommand::Health => {
            engine_protocol::write_frame(stream, &EngineResponse::Healthy)?;
            Ok(WorkerAction::Continue)
        }
        EngineCommand::Shutdown => {
            engine_protocol::write_frame(stream, &EngineResponse::Stopping)?;
            Ok(WorkerAction::Stop)
        }
        EngineCommand::Generate(request) => {
            let mut response = generate(engine, launch.spec.prompt_format, request);
            if let Err(error) = response.validate() {
                response = EngineResponse::Failed(EngineFailure {
                    kind: EngineFailureKind::Internal,
                    message: terminal_safe_message(&error.to_string()),
                });
            }
            engine_protocol::write_frame(stream, &response)?;
            Ok(WorkerAction::Continue)
        }
    }
}

fn generate(
    engine: &EmbeddedEngine,
    prompt_format: PromptFormat,
    request: GenerateRequest,
) -> EngineResponse {
    let messages = request
        .messages
        .iter()
        .map(|message| {
            EmbeddedMessage::new(
                match message.role {
                    ChatRole::System => EmbeddedRole::System,
                    ChatRole::User => EmbeddedRole::User,
                    ChatRole::Assistant => EmbeddedRole::Assistant,
                },
                message.content.clone(),
            )
        })
        .collect::<Vec<_>>();
    let generation_duration =
        remaining_generation_duration(request.expires_at_unix_ms, unix_millis());
    if generation_duration.is_zero() {
        return EngineResponse::Failed(EngineFailure {
            kind: EngineFailureKind::Deadline,
            message: "request expired while waiting for the embedded engine".into(),
        });
    }
    let deadline = Some(Instant::now() + generation_duration);
    let mut options = GenerationOptions {
        max_tokens: request.max_tokens,
        temperature: request.temperature,
        top_p: request.top_p,
        repeat_penalty: request.repeat_penalty,
        repeat_last_n: request.repeat_last_n,
        seed: request.seed,
        stop_token_ids: request.stop_token_ids,
        stop_strings: request.stop,
        deadline,
        ..GenerationOptions::default()
    };
    let result = match prompt_format.render_explicit(&messages) {
        None => engine.generate_chat(&messages, None, &options),
        Some(rendered) => {
            options.add_bos = false;
            engine.generate_rendered(&rendered, &options)
        }
    };
    match result {
        Ok(generation) => EngineResponse::Generated(GenerateResponse {
            content: generation.text,
            finish_reason: match generation.finish_reason {
                GenerationFinish::MaxTokens => FinishReason::Length,
                GenerationFinish::EndOfGeneration
                | GenerationFinish::StopToken(_)
                | GenerationFinish::StopString(_) => FinishReason::Stop,
            },
            prompt_tokens: generation.prompt_tokens,
            generated_tokens: generation.generated_tokens,
            elapsed_ms: u64::try_from(generation.elapsed.as_millis()).unwrap_or(u64::MAX),
        }),
        Err(error) => EngineResponse::Failed(EngineFailure {
            kind: if matches!(error, EmbeddedError::DeadlineExceeded) {
                EngineFailureKind::Deadline
            } else {
                EngineFailureKind::Model
            },
            message: terminal_safe_message(&error.to_string()),
        }),
    }
}

fn render_qwen_chatml(messages: &[EmbeddedMessage]) -> String {
    render_qwen_chatml_with_assistant_prefix(messages, "")
}

fn render_qwen35_no_thinking(messages: &[EmbeddedMessage]) -> String {
    let normalized = messages
        .iter()
        .map(|message| EmbeddedMessage::new(message.role, message.content.trim()))
        .collect::<Vec<_>>();
    render_qwen_chatml_with_assistant_prefix(&normalized, "<think>\n\n</think>\n\n")
}

fn render_qwen_chatml_with_assistant_prefix(
    messages: &[EmbeddedMessage],
    assistant_prefix: &str,
) -> String {
    let content_bytes = messages
        .iter()
        .map(|message| message.content.len())
        .sum::<usize>();
    let mut prompt =
        String::with_capacity(content_bytes + messages.len() * 32 + assistant_prefix.len() + 32);
    for message in messages {
        prompt.push_str("<|im_start|>");
        prompt.push_str(match message.role {
            EmbeddedRole::System => "system",
            EmbeddedRole::User => "user",
            EmbeddedRole::Assistant => "assistant",
        });
        prompt.push('\n');
        prompt.push_str(&message.content);
        prompt.push_str("<|im_end|>\n");
    }
    prompt.push_str("<|im_start|>assistant\n");
    prompt.push_str(assistant_prefix);
    prompt
}

fn validate_launch(path: &Path, identity: &str, launch: &EngineLaunch) -> Result<()> {
    if launch.schema != ENGINE_LAUNCH_SCHEMA
        || launch.runtime_policy_schema != ENGINE_RUNTIME_POLICY_SCHEMA
    {
        return Err(Error::Server(
            "unsupported embedded engine launch manifest".into(),
        ));
    }
    if launch.identity != identity
        || !valid_hex_key(&launch.identity)
        || !valid_hex_key(&launch.fingerprint)
        || launch.idle_timeout_seconds != ENGINE_IDLE_TIMEOUT.as_secs()
    {
        return Err(Error::Server(
            "embedded engine launch identity is invalid".into(),
        ));
    }
    for candidate in [
        path,
        &launch.executable_path,
        &launch.socket_path,
        &launch.key_path,
        &launch.spec.model_path,
    ] {
        if !candidate.is_absolute() || candidate.to_str().is_none() {
            return Err(Error::Server(
                "embedded engine launch paths must be absolute UTF-8 paths".into(),
            ));
        }
    }
    if launch
        .spec
        .lora_adapter
        .as_ref()
        .is_some_and(|adapter| !adapter.is_absolute() || adapter.to_str().is_none())
    {
        return Err(Error::Server(
            "embedded engine launch LoRA path is invalid".into(),
        ));
    }
    let runtime_directory = path
        .parent()
        .ok_or_else(|| Error::Server("embedded engine launch path has no parent".into()))?;
    if launch.socket_path.parent() != Some(runtime_directory)
        || launch.key_path.parent() != Some(runtime_directory)
    {
        return Err(Error::Server(
            "embedded engine launch files do not share a runtime directory".into(),
        ));
    }
    let executable = current_executable()?;
    if launch.executable_path != executable {
        return Err(Error::Server(
            "embedded engine launch executable does not match this process".into(),
        ));
    }
    let spec = launch.spec.normalized()?;
    if spec != launch.spec || fingerprint(&spec, &executable)? != launch.fingerprint {
        return Err(Error::Server(
            "embedded engine launch fingerprint is stale".into(),
        ));
    }
    Ok(())
}

fn fingerprint(spec: &EngineSpec, executable: &Path) -> Result<String> {
    let mut digest = Sha256::new();
    add_fingerprint_field(&mut digest, crate::VERSION.as_bytes());
    add_fingerprint_field(
        &mut digest,
        ENGINE_RUNTIME_POLICY_SCHEMA.to_string().as_bytes(),
    );
    add_file_fingerprint(&mut digest, executable)?;
    add_file_fingerprint(&mut digest, &spec.model_path)?;
    if let Some(adapter) = &spec.lora_adapter {
        add_fingerprint_field(&mut digest, b"lora");
        add_file_fingerprint(&mut digest, adapter)?;
    } else {
        add_fingerprint_field(&mut digest, b"no-lora");
    }
    let configuration = serde_json::to_vec(spec)?;
    add_fingerprint_field(&mut digest, &configuration);
    Ok(hex::encode(digest.finalize()))
}

fn add_file_fingerprint(digest: &mut Sha256, path: &Path) -> Result<()> {
    let metadata = fs::metadata(path)?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos());
    add_fingerprint_field(digest, path.as_os_str().as_encoded_bytes());
    add_fingerprint_field(digest, metadata.len().to_string().as_bytes());
    add_fingerprint_field(digest, modified.to_string().as_bytes());
    Ok(())
}

fn add_fingerprint_field(digest: &mut Sha256, value: &[u8]) {
    digest.update(value.len().to_be_bytes());
    digest.update(value);
}

fn canonical_regular_file(path: &Path, description: &str) -> Result<PathBuf> {
    let canonical = fs::canonicalize(path).map_err(|error| {
        Error::Model(format!(
            "could not resolve embedded {description} {}: {error}",
            path.display()
        ))
    })?;
    if !fs::metadata(&canonical)?.is_file() {
        return Err(Error::Model(format!(
            "embedded {description} is not a regular file: {}",
            canonical.display()
        )));
    }
    if canonical.to_str().is_none() {
        return Err(Error::Configuration(format!(
            "embedded {description} path must be valid UTF-8"
        )));
    }
    Ok(canonical)
}

fn current_executable() -> Result<PathBuf> {
    let path = env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|error| Error::Server(format!("could not resolve HowTo executable: {error}")))?;
    if path.to_str().is_none() {
        return Err(Error::Server(
            "HowTo executable path must be valid UTF-8".into(),
        ));
    }
    Ok(path)
}

fn healthy_with_retries(connection: &Connection, attempts: usize) -> bool {
    for attempt in 0..attempts.max(1) {
        if connection.is_healthy(HEALTH_TIMEOUT) {
            return true;
        }
        if attempt + 1 < attempts {
            thread::sleep(Duration::from_millis(150));
        }
    }
    false
}

fn failure_error(failure: EngineFailure) -> Error {
    match failure.kind {
        EngineFailureKind::Model => Error::Model(format!(
            "embedded engine generation failed: {}",
            failure.message
        )),
        EngineFailureKind::Deadline => Error::Server(format!(
            "embedded engine generation deadline elapsed: {}",
            failure.message
        )),
        EngineFailureKind::InvalidRequest
        | EngineFailureKind::Busy
        | EngineFailureKind::Internal => Error::Server(format!(
            "embedded engine rejected the request: {}",
            failure.message
        )),
    }
}

fn write_failure(stream: &mut UnixStream, kind: EngineFailureKind, message: &str) -> Result<()> {
    engine_protocol::write_frame(
        stream,
        &EngineResponse::Failed(EngineFailure {
            kind,
            message: terminal_safe_message(message),
        }),
    )
}

fn random_hex_key() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn valid_hex_key(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn read_private_key(path: &Path) -> Result<String> {
    ensure_private_regular_file(path)?;
    let bytes = fs::read(path)?;
    let key = String::from_utf8(bytes)
        .map_err(|_| Error::Server("embedded engine key is not valid UTF-8".into()))?;
    if !valid_hex_key(&key) {
        return Err(Error::Server("embedded engine key is malformed".into()));
    }
    Ok(key)
}

fn load_private_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    ensure_private_regular_file(path)?;
    serde_json::from_slice(&fs::read(path)?).map_err(Into::into)
}

fn write_private_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    write_atomic_private(path, &bytes)
}

fn write_atomic_private(path: &Path, contents: &[u8]) -> Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(Error::Server(format!(
                "refusing unsafe embedded engine file {}",
                path.display()
            )));
        }
    }
    let parent = path
        .parent()
        .ok_or_else(|| Error::Server(format!("runtime path {} has no parent", path.display())))?;
    let temporary = parent.join(format!(
        ".engine-{}-{:016x}.tmp",
        std::process::id(),
        rand::random::<u64>()
    ));
    let result = (|| -> Result<()> {
        write_private(&temporary, contents)?;
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    Ok(())
}

fn ensure_private_regular_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(Error::Server(format!(
            "refusing unsafe embedded engine file {}",
            path.display()
        )));
    }
    if metadata.uid() != Uid::current().as_raw() {
        return Err(Error::Server(format!(
            "embedded engine file is owned by another user: {}",
            path.display()
        )));
    }
    if metadata.mode() & 0o077 != 0 {
        return Err(Error::Server(format!(
            "embedded engine file is not owner-only: {}",
            path.display()
        )));
    }
    Ok(())
}

fn validate_private_socket(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        Error::Server(format!(
            "could not inspect embedded engine socket {}: {error}",
            path.display()
        ))
    })?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != Uid::current().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(Error::Server(format!(
            "refusing unsafe embedded engine socket {}",
            path.display()
        )));
    }
    Ok(())
}

fn open_private_file(path: &Path, truncate: bool) -> Result<File> {
    if path.exists() {
        ensure_private_regular_file(path)?;
    }
    Ok(OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(truncate)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?)
}

fn acquire_lock(path: &Path, acquisition: LockAcquisition) -> Result<Option<File>> {
    let lock = open_private_file(path, false)?;
    match acquisition {
        LockAcquisition::Blocking => FileExt::lock_exclusive(&lock)?,
        LockAcquisition::NonBlocking => match FileExt::try_lock_exclusive(&lock) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
            Err(error) => return Err(error.into()),
        },
    }
    Ok(Some(lock))
}

fn remove_runtime_file(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            Err(Error::Server(format!(
                "refusing to remove runtime directory {}",
                path.display()
            )))
        }
        Ok(_) => {
            fs::remove_file(path)?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn quarantine_file(path: &Path) -> Result<PathBuf> {
    let quarantined = path.with_extension(format!(
        "json.corrupt-{}-{}-{:08x}",
        unix_seconds(),
        std::process::id(),
        rand::random::<u32>()
    ));
    fs::rename(path, &quarantined)?;
    Ok(quarantined)
}

fn configure_worker_environment(command: &mut Command) {
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
    command.env("RUST_BACKTRACE", "1");
}

fn process_matches(state: &EngineState) -> bool {
    if state.pid == std::process::id()
        || !valid_hex_key(&state.identity)
        || state.launch_path.to_str().is_none()
        || state.executable_path.to_str().is_none()
    {
        return false;
    }
    let Ok(raw_pid) = i32::try_from(state.pid) else {
        return false;
    };
    if !process_is_alive(Pid::from_raw(raw_pid)) {
        return false;
    }
    process_arguments_match(state)
}

#[cfg(target_os = "linux")]
fn process_arguments_match(state: &EngineState) -> bool {
    use std::os::unix::ffi::OsStrExt;

    let bytes = match fs::read(format!("/proc/{}/cmdline", state.pid)) {
        Ok(bytes) => bytes,
        Err(_) => return false,
    };
    let arguments = bytes
        .split(|byte| *byte == 0)
        .filter(|argument| !argument.is_empty())
        .collect::<Vec<_>>();
    let expected = [
        OsStr::new(ENGINE_WORKER_ARGUMENT).as_bytes(),
        state.launch_path.as_os_str().as_bytes(),
        state.identity.as_bytes(),
    ];
    arguments
        .windows(expected.len())
        .any(|window| window == expected)
        && arguments
            .first()
            .is_some_and(|argument| *argument == state.executable_path.as_os_str().as_bytes())
}

#[cfg(not(target_os = "linux"))]
fn process_arguments_match(state: &EngineState) -> bool {
    let Some(ps) = ["/bin/ps", "/usr/bin/ps"]
        .into_iter()
        .find(|candidate| Path::new(candidate).is_file())
    else {
        return false;
    };
    let mut command = Command::new(ps);
    command.env_clear();
    let output = match command
        .args(["-ww", "-p", &state.pid.to_string(), "-o", "command="])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return false,
    };
    let command = String::from_utf8_lossy(&output.stdout);
    let executable_name = state
        .executable_path
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("howto");
    command.contains(executable_name)
        && command.contains(ENGINE_WORKER_ARGUMENT)
        && command.contains(&state.launch_path.to_string_lossy().to_string())
        && command.contains(&state.identity)
}

fn process_is_alive(pid: Pid) -> bool {
    matches!(signal::kill(pid, None), Ok(()) | Err(Errno::EPERM))
}

fn signal_owned(state: &EngineState, signal_value: Signal) -> Result<()> {
    if !process_matches(state) {
        return Ok(());
    }
    let pid =
        Pid::from_raw(i32::try_from(state.pid).map_err(|_| {
            Error::Server("embedded engine PID is outside the supported range".into())
        })?);
    match signal::kill(pid, signal_value) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(error) => Err(Error::Server(format!(
            "could not signal the embedded engine: {error}"
        ))),
    }
}

fn validate_socket_length(path: &Path) -> Result<()> {
    let length = path.as_os_str().as_encoded_bytes().len();
    let maximum = if cfg!(target_os = "macos") { 103 } else { 107 };
    if length > maximum {
        return Err(Error::Configuration(format!(
            "runtime path is too long for a Unix socket ({length} > {maximum}): {}; shorten HOWTO_HOME",
            path.display()
        )));
    }
    Ok(())
}

fn terminate_child(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn remaining_until(deadline: Instant) -> Option<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    (!remaining.is_zero()).then_some(remaining)
}

fn duration_seconds_rounded_up(duration: Duration) -> u64 {
    duration
        .as_secs()
        .saturating_add(u64::from(duration.subsec_nanos() != 0))
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}

fn remaining_generation_duration(expires_at_unix_ms: Option<u64>, now_unix_ms: u64) -> Duration {
    expires_at_unix_ms
        .map(|expires_at| Duration::from_millis(expires_at.saturating_sub(now_unix_ms)))
        .unwrap_or(MAX_GENERATION_DURATION)
        .min(MAX_GENERATION_DURATION)
}

fn open_log(path: &Path) -> Result<File> {
    if path.exists() {
        ensure_private_regular_file(path)?;
    }
    Ok(OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?)
}

fn rotate_log(path: &Path) -> Result<()> {
    if fs::metadata(path).is_ok_and(|metadata| metadata.len() > MAX_LOG_BYTES) {
        ensure_private_regular_file(path)?;
        let previous = path.with_extension("log.1");
        remove_runtime_file(&previous)?;
        fs::rename(path, previous)?;
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
        format!("\nrecent embedded engine log:\n{}", safe_log_text(&text))
    }
}

fn safe_log_text(value: &str) -> String {
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

struct SocketGuard(PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = remove_runtime_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::process::Command;
    use std::time::Duration;

    use fs2::FileExt;

    use super::{
        acquire_lock, configure_worker_environment, constant_time_eq, existing_worker_action,
        fingerprint, random_hex_key, remaining_generation_duration, render_qwen35_no_thinking,
        render_qwen_chatml, safe_log_text, write_private, EngineSpec, ExistingWorkerAction,
        GpuLayerPolicy, LockAcquisition, PromptFormat, LOG_TAIL_DISPLAY_BYTES,
        MAX_GENERATION_DURATION,
    };
    use crate::embedded::{ChatMessage, ChatRole};

    fn spec(model_path: std::path::PathBuf) -> EngineSpec {
        EngineSpec {
            model_path,
            lora_adapter: None,
            lora_scale: 1.0,
            context_size: 2_048,
            batch_size: 512,
            threads: 2,
            gpu_layers: GpuLayerPolicy::Cpu,
            use_mmap: true,
            prompt_format: PromptFormat::ModelDefault,
            max_tokens: 96,
            model_id: "test-model".into(),
        }
    }

    #[test]
    fn keys_are_random_lowercase_hex_and_compared_exactly() {
        let first = random_hex_key();
        let second = random_hex_key();
        assert_eq!(first.len(), 64);
        assert_ne!(first, second);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(constant_time_eq(first.as_bytes(), first.as_bytes()));
        assert!(!constant_time_eq(first.as_bytes(), second.as_bytes()));
        assert!(!constant_time_eq(b"short", first.as_bytes()));
    }

    #[test]
    fn qwen35_no_thinking_prompt_is_exact() {
        let messages = vec![
            ChatMessage::new(ChatRole::System, " one line only\n"),
            ChatMessage::new(ChatRole::User, "  list files  "),
        ];
        assert_eq!(
            render_qwen35_no_thinking(&messages),
            "<|im_start|>system\none line only<|im_end|>\n<|im_start|>user\nlist files<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n"
        );
    }

    #[test]
    fn compact_qwen_chatml_prompt_is_exact() {
        let messages = vec![
            ChatMessage::new(ChatRole::System, "one line only\n"),
            ChatMessage::new(ChatRole::User, "list files"),
        ];
        assert_eq!(
            render_qwen_chatml(&messages),
            "<|im_start|>system\none line only\n<|im_end|>\n<|im_start|>user\nlist files<|im_end|>\n<|im_start|>assistant\n"
        );
    }

    #[test]
    fn queued_generation_budget_expires_before_work_starts() {
        assert_eq!(
            remaining_generation_duration(Some(10_250), 10_000),
            Duration::from_millis(250)
        );
        assert!(remaining_generation_duration(Some(9_999), 10_000).is_zero());
        assert_eq!(
            remaining_generation_duration(Some(u64::MAX), 10_000),
            MAX_GENERATION_DURATION
        );
        assert_eq!(
            remaining_generation_duration(None, 10_000),
            MAX_GENERATION_DURATION
        );
    }

    #[test]
    fn fingerprint_covers_prompt_profile_and_lora() {
        let directory = tempfile::tempdir().unwrap();
        let model = directory.path().join("model.gguf");
        let adapter = directory.path().join("adapter.gguf");
        fs::write(&model, b"model").unwrap();
        fs::write(&adapter, b"adapter").unwrap();
        let executable = std::env::current_exe().and_then(fs::canonicalize).unwrap();

        let base = spec(fs::canonicalize(&model).unwrap());
        let base_fingerprint = fingerprint(&base, &executable).unwrap();
        let mut qwen = base.clone();
        qwen.prompt_format = PromptFormat::Qwen35NoThinking;
        assert_ne!(base_fingerprint, fingerprint(&qwen, &executable).unwrap());

        let mut compact_qwen = base.clone();
        compact_qwen.prompt_format = PromptFormat::QwenChatMl;
        assert_ne!(
            base_fingerprint,
            fingerprint(&compact_qwen, &executable).unwrap()
        );
        assert_ne!(
            fingerprint(&qwen, &executable).unwrap(),
            fingerprint(&compact_qwen, &executable).unwrap()
        );

        let mut lora = base;
        lora.lora_adapter = Some(fs::canonicalize(adapter).unwrap());
        assert_ne!(base_fingerprint, fingerprint(&lora, &executable).unwrap());
    }

    #[test]
    fn private_files_are_owner_only_and_symlinks_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("engine.key");
        write_private(&path, b"secret").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let target = directory.path().join("target");
        fs::write(&target, b"untouched").unwrap();
        let link = directory.path().join("link");
        symlink(&target, &link).unwrap();
        assert!(write_private(&link, b"replacement").is_err());
        assert_eq!(fs::read(&target).unwrap(), b"untouched");
    }

    #[test]
    fn nonblocking_engine_lock_reports_contention() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("engine.lock");
        let held = acquire_lock(&path, LockAcquisition::Blocking)
            .unwrap()
            .unwrap();
        assert!(acquire_lock(&path, LockAcquisition::NonBlocking)
            .unwrap()
            .is_none());
        FileExt::unlock(&held).unwrap();
        assert!(acquire_lock(&path, LockAcquisition::NonBlocking)
            .unwrap()
            .is_some());
    }

    #[test]
    fn sequential_worker_is_reused_or_reported_busy_without_interrupting_it() {
        assert_eq!(
            existing_worker_action(true, false, true),
            ExistingWorkerAction::Reuse
        );
        assert_eq!(
            existing_worker_action(false, false, true),
            ExistingWorkerAction::Busy
        );
        assert_eq!(
            existing_worker_action(false, true, true),
            ExistingWorkerAction::Restart
        );
        assert_eq!(
            existing_worker_action(true, false, false),
            ExistingWorkerAction::Restart
        );
    }

    #[test]
    fn worker_environment_is_an_allowlist() {
        let mut command = Command::new("howto");
        command.env("AWS_SECRET_ACCESS_KEY", "secret");
        command.env("DYLD_INSERT_LIBRARIES", "/tmp/inject");
        configure_worker_environment(&mut command);
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
            "RUST_BACKTRACE",
        ];
        for (key, _) in command.get_envs() {
            assert!(allowed.contains(&key.to_string_lossy().as_ref()));
        }
    }

    #[test]
    fn crash_log_text_is_terminal_safe_and_bounded() {
        let text = format!("\u{1b}[31mboom\u{202e}{}", "x".repeat(16_384));
        let safe = safe_log_text(&text);
        assert!(safe.len() <= LOG_TAIL_DISPLAY_BYTES);
        assert!(!safe.contains('\u{1b}'));
        assert!(!safe.contains('\u{202e}'));
        assert!(safe.contains("\\u{1b}"));
    }

    #[test]
    fn engine_spec_validation_does_not_require_loading_a_model() {
        let mut value = spec("/tmp/model.gguf".into());
        assert!(value.validate().is_ok());
        value.lora_scale = 0.0;
        assert!(value.validate().is_err());
        value.lora_scale = 1.0;
        value.context_size = 96;
        assert!(value.validate().is_err());
        let _ = Duration::from_secs(1);
    }
}
