use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::cli::{self, ConfigCommand, Invocation, ModelCommand, QueryOptions, ServerCommand};
use crate::config::{self, Config};
use crate::error::{Error, Result};
use crate::execute;
use crate::inference::Generator;
use crate::model::{self, Installer, ModelLocation, DEFAULT_MODEL};
use crate::paths::Paths;
use crate::platform::Platform;
use crate::runtime::{self, Manager};
use crate::safety::{Assessment, Risk};

pub fn run(invocation: Invocation) -> Result<i32> {
    match invocation {
        Invocation::Help => {
            cli::print_help();
            Ok(0)
        }
        Invocation::Version => {
            println!("howto {}", crate::VERSION);
            Ok(0)
        }
        Invocation::Query(options) => run_query(options),
        Invocation::Doctor { json, deep } => run_doctor(json, deep),
        Invocation::Model(command) => run_model(command),
        Invocation::Server(command) => run_server(command),
        Invocation::Config(command) => run_config(command),
    }
}

fn run_query(options: QueryOptions) -> Result<i32> {
    runtime::refuse_elevated()?;
    let prompt = read_prompt(options.words)?;
    let paths = Paths::discover()?;
    paths.create()?;
    let config = config::load(&paths.config_file())?;
    let platform = Platform::current();

    let model_path = if config.server_url.is_some() {
        PathBuf::new()
    } else {
        ensure_model(&config, &paths, true)?.path().to_path_buf()
    };

    let manager = Manager::new(&config, &paths);
    if config.server_url.is_none() && manager.status()?.is_none() && !options.quiet {
        eprintln!("Starting the local model…");
    }
    let connection = manager.ensure(&model_path)?;
    let generation =
        Generator::new(&connection, &config, platform).generate(&prompt, options.count)?;
    let assessments = generation
        .commands
        .iter()
        .map(|command| assess(command, platform))
        .collect::<Vec<_>>();

    if options.quiet {
        let assessment = &assessments[0];
        if assessment.risk != Risk::NoKnownRisk {
            return Err(Error::Execution(format!(
                "quiet output is blocked for a {} command; review it without --quiet",
                risk_name(assessment.risk)
            )));
        }
        println!("{}", generation.commands[0]);
    } else if options.json {
        let commands = generation
            .commands
            .iter()
            .zip(&assessments)
            .map(|(command, assessment)| assessment_json(command, assessment))
            .collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "request": prompt,
                "platform": platform.name(),
                "commands": commands,
                "elapsed_ms": generation.elapsed_ms,
                "provider": if connection.managed { "local" } else { "configured" },
            }))?
        );
    } else {
        print_commands(&generation.commands, &assessments);
    }
    if options.timing && !options.json {
        eprintln!(
            "Generated in {:.2}s",
            generation.elapsed_ms as f64 / 1_000.0
        );
    }

    let selected =
        if !options.quiet && generation.commands.len() > 1 && (options.execute || options.copy) {
            select_command(generation.commands.len())?
        } else {
            0
        };
    let command = &generation.commands[selected];
    let assessment = &assessments[selected];

    if options.copy {
        execute::copy_to_clipboard(command, platform)?;
        if !options.quiet && !options.json {
            eprintln!("Copied command to the clipboard.");
        }
    }
    if !options.execute {
        return Ok(0);
    }
    eprintln!("Selected command:\n  {command}");
    match assessment.risk {
        Risk::Danger => {
            return Err(Error::Execution(
                "DANGER commands are never executed by HowTo".into(),
            ));
        }
        Risk::Unknown => {
            return Err(Error::Execution(
                "commands the safety parser cannot fully understand are never executed".into(),
            ));
        }
        Risk::Caution => {
            if !execute::confirm("Type RUN to execute this caution command: ", "RUN")? {
                eprintln!("Not run.");
                return Ok(0);
            }
        }
        Risk::NoKnownRisk => {
            if !execute::confirm("Run this command? Type y to continue: ", "y")? {
                eprintln!("Not run.");
                return Ok(0);
            }
        }
    }
    execute::run(command, &config.shell_path)
}

fn read_prompt(words: Vec<String>) -> Result<String> {
    if !words.is_empty() {
        let prompt = words.join(" ").trim().to_owned();
        if prompt.len() > 8_192 {
            return Err(Error::Usage(
                "request is too long (maximum 8192 bytes)".into(),
            ));
        }
        if prompt.is_empty() {
            return Err(Error::Usage("tell HowTo what you want to do".into()));
        }
        return Ok(prompt);
    }
    if io::stdin().is_terminal() {
        eprint!("What should I do? › ");
        io::stderr().flush()?;
        let mut prompt = String::new();
        io::stdin().read_line(&mut prompt)?;
        let prompt = prompt.trim().to_owned();
        if prompt.len() > 8_192 {
            return Err(Error::Usage(
                "request is too long (maximum 8192 bytes)".into(),
            ));
        }
        if prompt.is_empty() {
            return Err(Error::Usage("tell HowTo what you want to do".into()));
        }
        return Ok(prompt);
    }
    let mut bytes = Vec::new();
    io::stdin().take(8_193).read_to_end(&mut bytes)?;
    if bytes.len() > 8_192 {
        return Err(Error::Usage(
            "request on stdin is too long (maximum 8192 bytes)".into(),
        ));
    }
    let prompt = String::from_utf8(bytes)
        .map_err(|_| Error::Usage("request on stdin must be valid UTF-8".into()))?;
    let prompt = prompt.trim().to_owned();
    if prompt.is_empty() {
        return Err(Error::Usage("tell HowTo what you want to do".into()));
    }
    Ok(prompt)
}

fn assess(command: &str, platform: Platform) -> Assessment {
    let safety_platform = match platform {
        Platform::Macos => crate::safety::Platform::MacOs,
        Platform::Linux => crate::safety::Platform::Linux,
    };
    crate::safety::assess_for_platform(command, safety_platform)
}

fn print_commands(commands: &[String], assessments: &[Assessment]) {
    if commands.len() == 1 {
        println!("{}", commands[0]);
        print_findings(&assessments[0]);
        return;
    }
    for (index, (command, assessment)) in commands.iter().zip(assessments).enumerate() {
        println!("{}. {}", index + 1, command);
        if assessment.risk != Risk::NoKnownRisk {
            eprintln!("   {}", risk_name(assessment.risk));
        }
        print_findings(assessment);
    }
}

fn print_findings(assessment: &Assessment) {
    if assessment.risk == Risk::NoKnownRisk {
        return;
    }
    eprintln!("{}:", risk_name(assessment.risk));
    for finding in &assessment.findings {
        eprintln!("  - {} [{}]", finding.message, finding.rule_id);
    }
    if assessment.risk == Risk::Danger {
        eprintln!("  HowTo will not execute this command.");
    } else if assessment.risk == Risk::Unknown {
        eprintln!("  The safety parser could not prove enough about this command to execute it.");
    }
}

fn risk_name(risk: Risk) -> &'static str {
    match risk {
        Risk::NoKnownRisk => "NO_KNOWN_RISK",
        Risk::Caution => "CAUTION",
        Risk::Danger => "DANGER",
        Risk::Unknown => "UNKNOWN",
    }
}

fn assessment_json(command: &str, assessment: &Assessment) -> Value {
    let findings = assessment
        .findings
        .iter()
        .map(|finding| {
            json!({
                "rule": finding.rule_id,
                "severity": risk_name(finding.severity),
                "message": finding.message,
                "span": {"start": finding.span.start, "end": finding.span.end},
            })
        })
        .collect::<Vec<_>>();
    json!({
        "command": command,
        "risk": risk_name(assessment.risk),
        "findings": findings,
    })
}

fn select_command(count: usize) -> Result<usize> {
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return Err(Error::Execution(
            "selecting from multiple commands requires an interactive terminal".into(),
        ));
    }
    eprint!("Choose a command [1-{count}]: ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    let choice = answer
        .trim()
        .parse::<usize>()
        .map_err(|_| Error::Execution("invalid command selection".into()))?;
    if !(1..=count).contains(&choice) {
        return Err(Error::Execution("command selection is out of range".into()));
    }
    Ok(choice - 1)
}

fn ensure_model(config: &Config, paths: &Paths, allow_prompt: bool) -> Result<ModelLocation> {
    if let Some(location) = model::resolve(config, paths)? {
        return Ok(location);
    }
    if !allow_prompt || !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return Err(Error::Model(format!(
            "the local model is not installed; run `howto model install --yes` (downloads {:.0} MiB)",
            DEFAULT_MODEL.size as f64 / 1_048_576.0
        )));
    }
    eprint!(
        "HowTo needs its local model ({:.0} MiB). Download it now? [Y/n] ",
        DEFAULT_MODEL.size as f64 / 1_048_576.0
    );
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if !matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "" | "y" | "yes"
    ) {
        return Err(Error::Model("model download declined".into()));
    }
    install_model(paths).map(ModelLocation::Managed)
}

fn install_model(paths: &Paths) -> Result<PathBuf> {
    eprintln!(
        "Downloading {} from Hugging Face (resume supported)…",
        DEFAULT_MODEL.display_name
    );
    let mut last_percent = u64::MAX;
    let path = Installer::new()?.install(paths, |downloaded, total| {
        let percent = downloaded.saturating_mul(100) / total.max(1);
        if percent != last_percent {
            eprint!(
                "\r  {:>3}%  {:.1}/{:.1} MiB",
                percent,
                downloaded as f64 / 1_048_576.0,
                total as f64 / 1_048_576.0
            );
            let _ = io::stderr().flush();
            last_percent = percent;
        }
    })?;
    eprintln!("\nVerified SHA-256 and installed {}.", path.display());
    Ok(path)
}

fn run_model(command: ModelCommand) -> Result<i32> {
    runtime::refuse_elevated()?;
    let paths = Paths::discover()?;
    paths.create()?;
    let config = config::load(&paths.config_file())?;
    match command {
        ModelCommand::Install { yes } => {
            if let Some(location) = model::resolve(&config, &paths)? {
                match location {
                    ModelLocation::Configured(_) | ModelLocation::Packaged(_) => {
                        eprintln!(
                            "Model is already available at {}.",
                            location.path().display()
                        );
                        return Ok(0);
                    }
                    ModelLocation::Managed(ref path)
                        if model::verify_sha256(path, DEFAULT_MODEL.sha256)? =>
                    {
                        eprintln!("Model is already verified at {}.", path.display());
                        return Ok(0);
                    }
                    ModelLocation::Managed(ref path) => {
                        eprintln!(
                            "The managed model at {} failed verification and will be replaced.",
                            path.display()
                        );
                    }
                }
            }
            if !yes {
                if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
                    return Err(Error::Model(
                        "non-interactive install requires `howto model install --yes`".into(),
                    ));
                }
                eprint!(
                    "Download {:.0} MiB to {}? [y/N] ",
                    DEFAULT_MODEL.size as f64 / 1_048_576.0,
                    paths.models_dir().display()
                );
                io::stderr().flush()?;
                let mut answer = String::new();
                io::stdin().read_line(&mut answer)?;
                if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                    eprintln!("Not installed.");
                    return Ok(0);
                }
            }
            install_model(&paths)?;
            Ok(0)
        }
        ModelCommand::Status { json, deep } => print_model_status(&config, &paths, json, deep),
    }
}

fn print_model_status(
    config: &Config,
    paths: &Paths,
    json_output: bool,
    deep: bool,
) -> Result<i32> {
    let location = model::resolve(config, paths)?;
    let Some(location) = location else {
        if json_output {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({"installed": false}))?
            );
        } else {
            println!("Model: not installed");
        }
        return Ok(1);
    };
    let path = location.path();
    let size = fs::metadata(path)?.len();
    let verified = if deep
        && size == DEFAULT_MODEL.size
        && matches!(
            &location,
            ModelLocation::Managed(_) | ModelLocation::Packaged(_)
        ) {
        Some(model::verify_sha256(path, DEFAULT_MODEL.sha256)?)
    } else {
        None
    };
    let manifest = matches!(
        &location,
        ModelLocation::Managed(_) | ModelLocation::Packaged(_)
    )
    .then_some(DEFAULT_MODEL.id);
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "installed": true,
                "path": path,
                "bytes": size,
                "sha256_verified": verified,
                "manifest": manifest,
            }))?
        );
    } else {
        println!("Model: {}", path.display());
        println!("Size:  {:.1} MiB", size as f64 / 1_048_576.0);
        if let Some(verified) = verified {
            println!("SHA-256: {}", if verified { "verified" } else { "FAILED" });
        } else if deep {
            println!("SHA-256: not checked (size differs from the bundled manifest)");
        }
    }
    Ok(i32::from(verified == Some(false)))
}

fn run_server(command: ServerCommand) -> Result<i32> {
    runtime::refuse_elevated()?;
    let paths = Paths::discover()?;
    paths.create()?;
    let config = config::load(&paths.config_file())?;
    let manager = Manager::new(&config, &paths);
    match command {
        ServerCommand::Status { json } => {
            let state = manager.status()?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "running": state.is_some(),
                        "state": state,
                    }))?
                );
            } else if let Some(state) = state {
                println!("Server: running (pid {})", state.pid);
                println!("Model:  {}", state.model_path.display());
                println!("Socket: {}", state.socket_path.display());
            } else {
                println!("Server: stopped");
            }
            Ok(0)
        }
        ServerCommand::Stop { json } => {
            let stopped = manager.stop()?;
            if json {
                println!("{}", json!({"stopped": stopped}));
            } else if stopped {
                println!("Stopped the HowTo model server.");
            } else {
                println!("Server was not running.");
            }
            Ok(0)
        }
    }
}

fn run_doctor(json_output: bool, deep: bool) -> Result<i32> {
    runtime::refuse_elevated()?;
    let paths = Paths::discover()?;
    paths.create()?;
    let config = config::load(&paths.config_file())?;
    let external = config.server_url.is_some();
    let model = if external {
        None
    } else {
        model::resolve(&config, &paths)?
    };
    let runtime_path = if external {
        None
    } else {
        runtime::resolve_llama_server(&config).ok()
    };
    let runtime_usable = runtime_path.as_deref().map(runtime::probe_llama_server);
    let model_verified = if deep {
        model
            .as_ref()
            .filter(|location| {
                !matches!(location, ModelLocation::Configured(_))
                    && fs::metadata(location.path())
                        .is_ok_and(|meta| meta.len() == DEFAULT_MODEL.size)
            })
            .map(|location| model::verify_sha256(location.path(), DEFAULT_MODEL.sha256))
            .transpose()?
    } else {
        None
    };
    let manager = Manager::new(&config, &paths);
    let provider_healthy = manager.configured_provider_healthy()?;
    let server = manager.status()?;
    let ready = provider_healthy.unwrap_or_else(|| {
        model.is_some() && runtime_usable == Some(true) && model_verified != Some(false)
    });
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "ready": ready,
                "version": crate::VERSION,
                "platform": Platform::current().name(),
                "provider": if external { "configured" } else { "local" },
                "server_url": config.server_url,
                "provider_healthy": provider_healthy,
                "model_path": model.as_ref().map(|location| location.path()),
                "model_sha256_verified": model_verified,
                "llama_server_path": runtime_path,
                "llama_server_usable": runtime_usable,
                "server_running": server.is_some(),
                "config_path": paths.config_file(),
            }))?
        );
    } else {
        println!("HowTo {} — {}", crate::VERSION, Platform::current().name());
        if let Some(url) = &config.server_url {
            println!("Provider: configured endpoint {url}");
            println!(
                "Reachable: {}",
                if provider_healthy == Some(true) {
                    "yes"
                } else {
                    "no"
                }
            );
            println!("Privacy: requests may leave this machine when using a configured endpoint");
        } else {
            println!(
                "Model:   {}",
                model.as_ref().map_or_else(
                    || "missing".into(),
                    |location| location.path().display().to_string()
                )
            );
            if let Some(verified) = model_verified {
                println!(
                    "Model hash: {}",
                    if verified { "verified" } else { "FAILED" }
                );
            }
            println!(
                "Runtime: {}",
                runtime_path.as_ref().map_or_else(
                    || "llama-server missing or invalid".into(),
                    |path| path.display().to_string()
                )
            );
            if runtime_path.is_some() {
                println!(
                    "Runtime check: {}",
                    if runtime_usable == Some(true) {
                        "llama-server --version succeeded"
                    } else {
                        "FAILED"
                    }
                );
            }
        }
        println!(
            "Server:  {}",
            server.map_or_else(
                || "stopped".into(),
                |state| format!("running (pid {})", state.pid)
            )
        );
        println!("Config:  {}", paths.config_file().display());
        println!(
            "Status:  {}",
            if ready { "ready" } else { "needs attention" }
        );
    }
    Ok(i32::from(!ready))
}

fn run_config(command: ConfigCommand) -> Result<i32> {
    runtime::refuse_elevated()?;
    let paths = Paths::discover()?;
    paths.create()?;
    let path = paths.config_file();
    let mut configuration = config::load(&path)?;
    match command {
        ConfigCommand::List { json } => {
            if json {
                println!("{}", serde_json::to_string_pretty(&configuration)?);
            } else {
                print_config(&configuration);
            }
        }
        ConfigCommand::Path => println!("{}", path.display()),
        ConfigCommand::Get { key } => println!("{}", config_value(&configuration, &key)?),
        ConfigCommand::Set { key, value } => {
            set_config_value(&mut configuration, &key, Some(&value))?;
            config::save(&path, &configuration)?;
            println!("Set {key}.");
        }
        ConfigCommand::Unset { key } => {
            set_config_value(&mut configuration, &key, None)?;
            config::save(&path, &configuration)?;
            println!("Reset {key} to its default.");
        }
    }
    Ok(0)
}

fn config_value(config: &Config, key: &str) -> Result<String> {
    match key {
        "model_path" => Ok(optional_path(&config.model_path)),
        "llama_server_path" => Ok(optional_path(&config.llama_server_path)),
        "server_url" => Ok(config.server_url.clone().unwrap_or_else(|| "null".into())),
        "threads" => Ok(config.threads.to_string()),
        "context_size" => Ok(config.context_size.to_string()),
        "max_tokens" => Ok(config.max_tokens.to_string()),
        "startup_timeout_seconds" => Ok(config.startup_timeout_seconds.to_string()),
        "shell_path" => Ok(config.shell_path.display().to_string()),
        "model_id" => Ok(config.model_id.clone()),
        _ => Err(Error::Usage(format!("unknown config key `{key}`"))),
    }
}

fn optional_path(path: &Option<PathBuf>) -> String {
    path.as_ref()
        .map_or_else(|| "null".into(), |path| path.display().to_string())
}

fn set_config_value(config: &mut Config, key: &str, value: Option<&str>) -> Result<()> {
    let defaults = Config::default();
    match key {
        "model_path" => config.model_path = value.map(PathBuf::from),
        "llama_server_path" => config.llama_server_path = value.map(PathBuf::from),
        "server_url" => config.server_url = value.map(str::to_owned),
        "threads" => config.threads = parse_or_default(value, defaults.threads, key)?,
        "context_size" => {
            config.context_size = parse_or_default(value, defaults.context_size, key)?;
        }
        "max_tokens" => config.max_tokens = parse_or_default(value, defaults.max_tokens, key)?,
        "startup_timeout_seconds" => {
            config.startup_timeout_seconds =
                parse_or_default(value, defaults.startup_timeout_seconds, key)?;
        }
        "shell_path" => {
            config.shell_path = value.map_or(defaults.shell_path, PathBuf::from);
        }
        "model_id" => config.model_id = value.map_or(defaults.model_id, str::to_owned),
        _ => return Err(Error::Usage(format!("unknown config key `{key}`"))),
    }
    config.validate()
}

fn parse_or_default<T>(value: Option<&str>, default: T, key: &str) -> Result<T>
where
    T: std::str::FromStr,
{
    value.map_or(Ok(default), |value| {
        value
            .parse()
            .map_err(|_| Error::Usage(format!("invalid value for `{key}`: {value}")))
    })
}

fn print_config(config: &Config) {
    for key in [
        "model_path",
        "llama_server_path",
        "server_url",
        "threads",
        "context_size",
        "max_tokens",
        "startup_timeout_seconds",
        "shell_path",
        "model_id",
    ] {
        if let Ok(value) = config_value(config, key) {
            println!("{key} = {value}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{config_value, read_prompt, set_config_value};
    use crate::config::Config;

    #[test]
    fn config_updates_are_validated() {
        let mut config = Config::default();
        set_config_value(&mut config, "threads", Some("3")).unwrap();
        assert_eq!(config.threads, 3);
        assert_eq!(config_value(&config, "threads").unwrap(), "3");
        assert!(set_config_value(&mut config, "threads", Some("0")).is_err());
    }

    #[test]
    fn unset_restores_default() {
        let mut config = Config {
            model_id: "custom".into(),
            ..Config::default()
        };
        set_config_value(&mut config, "model_id", None).unwrap();
        assert_eq!(config.model_id, Config::default().model_id);
    }

    #[test]
    fn empty_argument_request_is_rejected_before_setup() {
        assert!(read_prompt(vec!["   ".into()]).is_err());
    }
}
