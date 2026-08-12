use std::env;
use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::cli::{
    self, ConfigCommand, Invocation, ModelCommand, QueryOptions, ServerCommand, SetupOptions,
    ShellCommand,
};
use crate::config::{self, Config};
use crate::error::{Error, Result};
use crate::execute;
use crate::inference::Generator;
use crate::model::{self, Installer, ModelLocation, DEFAULT_MODEL};
use crate::paths::Paths;
use crate::platform::Platform;
use crate::runtime::{self, Manager};
use crate::safety::{Assessment, Risk};
use crate::{setup, shell};

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
        Invocation::Setup(options) => run_setup(options),
        Invocation::Query(options) => run_query(options),
        Invocation::Doctor { json, deep } => run_doctor(json, deep),
        Invocation::Model(command) => run_model(command),
        Invocation::Server(command) => run_server(command),
        Invocation::Config(command) => run_config(command),
        Invocation::Shell(command) => run_shell(command),
    }
}

fn run_query(options: QueryOptions) -> Result<i32> {
    runtime::refuse_elevated()?;
    let paths = Paths::discover()?;
    paths.create()?;
    let config = config::load(&paths.config_file())?;
    ensure_setup_for_query(&options, &paths, &config)?;
    let prompt = read_prompt(options.words)?;
    let platform = Platform::current();

    let model_path = if config.server_url.is_some() {
        PathBuf::new()
    } else {
        ensure_model(&config, &paths)?.path().to_path_buf()
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

    if let Err(error) = shell::discard_pending(&paths) {
        if io::stderr().is_terminal() && !options.json && !options.quiet {
            eprintln!("Warning: could not clear the previous Tab suggestion: {error}");
        }
    }

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

    if !options.quiet
        && !options.json
        && !options.execute
        && generation.commands.len() == 1
        && io::stdout().is_terminal()
        && io::stderr().is_terminal()
        && matches!(assessment.risk, Risk::NoKnownRisk | Risk::Caution)
    {
        match store_pending_for_configured_shell(&paths, command) {
            Ok(true) if config.show_tab_hint => {
                eprintln!("Press Tab at an empty prompt to edit this command.");
            }
            Ok(true) => {}
            Ok(false) => {}
            Err(error) => eprintln!("Warning: could not prepare the Tab shortcut: {error}"),
        }
    }

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

fn store_pending_for_configured_shell(paths: &Paths, command: &str) -> Result<bool> {
    let _lock = setup::Lock::acquire(paths)?;
    let Some(receipt) = setup::load(paths)? else {
        return Ok(false);
    };
    let configured = receipt
        .shell
        .as_deref()
        .map(str::parse::<shell::Kind>)
        .transpose()?;
    if shell::session_kind()? != configured || configured.is_none() {
        return Ok(false);
    }
    shell::store_pending(paths, command)
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

fn ensure_setup_for_query(options: &QueryOptions, paths: &Paths, config: &Config) -> Result<()> {
    if setup::load(paths)?.is_some() {
        let still_complete = {
            let _lock = setup::Lock::acquire(paths)?;
            if let Some(mut current) = setup::load(paths)? {
                let refresh = (|| -> Result<()> {
                    if let Some(kind) = current
                        .shell
                        .as_deref()
                        .map(str::parse::<shell::Kind>)
                        .transpose()?
                    {
                        shell::refresh_adapter(paths, kind)?;
                        if current.shell_integration_schema != Some(setup::SHELL_INTEGRATION_SCHEMA)
                        {
                            current.shell_integration_schema =
                                Some(setup::SHELL_INTEGRATION_SCHEMA);
                            setup::save(paths, &current)?;
                        }
                    }
                    Ok(())
                })();
                if let Err(error) = refresh {
                    if io::stderr().is_terminal() && !options.json && !options.quiet {
                        eprintln!(
                            "Warning: could not refresh the optional Tab integration: {error}"
                        );
                    }
                }
                true
            } else {
                false
            }
        };
        if still_complete {
            return Ok(());
        }
    }
    if options.json || options.quiet || !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return Err(Error::Configuration(
            "HowTo setup has not been completed; run `howto setup` first".into(),
        ));
    }
    let setup_prompt = format!(
        "HowTo setup has not been completed. Run it now? This may download the {:.0} MiB local model. [Y/n] ",
        DEFAULT_MODEL.size as f64 / 1_048_576.0
    );
    if !prompt_yes_no(&setup_prompt, true)? {
        return Err(Error::Configuration(
            "setup was declined; run `howto setup` when you are ready".into(),
        ));
    }
    let code = run_setup_workflow(
        paths,
        config,
        &SetupOptions {
            yes: false,
            no_shell: false,
            shell: None,
        },
        true,
    )?;
    if code == 0 {
        Ok(())
    } else {
        Err(Error::Configuration(
            "setup did not complete; run `howto setup` to try again".into(),
        ))
    }
}

fn run_setup(options: SetupOptions) -> Result<i32> {
    runtime::refuse_elevated()?;
    let paths = Paths::discover()?;
    paths.create()?;
    let config = config::load(&paths.config_file())?;
    run_setup_workflow(&paths, &config, &options, false)
}

fn run_setup_workflow(
    paths: &Paths,
    config: &Config,
    options: &SetupOptions,
    model_download_confirmed: bool,
) -> Result<i32> {
    let _lock = setup::Lock::acquire(paths)?;
    let (previous, quarantined) = setup::load_for_setup(paths)?;
    if let Some(path) = quarantined {
        eprintln!(
            "Warning: moved malformed setup state to {} before repairing setup.",
            path.display()
        );
    }

    if let Some(url) = &config.server_url {
        eprintln!("Using the configured provider at {url}; no local model download is needed.");
    } else {
        let runtime_path = runtime::resolve_llama_server(config)?;
        if !runtime::probe_llama_server(&runtime_path) {
            return Err(Error::Dependency(format!(
                "{} did not pass the llama-server version check",
                runtime_path.display()
            )));
        }
        eprintln!("Runtime found at {}.", runtime_path.display());
        let mut install_required = false;
        match model::resolve(config, paths)? {
            Some(ModelLocation::Managed(path)) => {
                if model::verify_sha256(&path, DEFAULT_MODEL.sha256)? {
                    eprintln!("Model is already verified at {}.", path.display());
                } else {
                    eprintln!(
                        "The managed model at {} failed verification and will be replaced.",
                        path.display()
                    );
                    install_required = true;
                }
            }
            Some(ModelLocation::Packaged(path)) => {
                if !model::verify_sha256(&path, DEFAULT_MODEL.sha256)? {
                    return Err(Error::Model(format!(
                        "the packaged model at {} failed SHA-256 verification; repair or remove that package before setup",
                        path.display()
                    )));
                }
                eprintln!("Packaged model is verified at {}.", path.display());
            }
            Some(location @ ModelLocation::Configured(_)) => {
                eprintln!(
                    "Model is already available at {}.",
                    location.path().display()
                );
            }
            None => install_required = true,
        }
        if install_required {
            let confirmed = options.yes
                || model_download_confirmed
                || (interactive()
                    && prompt_yes_no(
                        &format!(
                            "Download the local model ({:.0} MiB)? [Y/n] ",
                            DEFAULT_MODEL.size as f64 / 1_048_576.0
                        ),
                        true,
                    )?);
            if !confirmed {
                eprintln!("Setup was not completed.");
                return Ok(1);
            }
            install_model(paths)?;
        }
    }

    let previous_shell = previous
        .as_ref()
        .and_then(|receipt| receipt.shell.as_deref())
        .map(str::parse::<shell::Kind>)
        .transpose()?;
    let mut shell_changes = Vec::new();
    let previous_startup_files = previous
        .as_ref()
        .map(|receipt| receipt.shell_startup_files.as_slice())
        .unwrap_or_default();
    let mut chosen_startup_files = Vec::new();
    let chosen_shell = if options.no_shell {
        for kind in shell::Kind::ALL {
            let recorded = if previous_shell == Some(kind) {
                previous_startup_files
            } else {
                &[]
            };
            let has_artifacts = if previous_shell == Some(kind) {
                true
            } else {
                match shell::has_managed_artifacts(paths, kind) {
                    Ok(value) => value,
                    Err(error) => return Err(with_shell_rollback(error, &shell_changes)),
                }
            };
            if previous_shell == Some(kind) || has_artifacts {
                let result = match shell::disable_recorded(paths, kind, recorded) {
                    Ok(result) => result,
                    Err(error) => return Err(with_shell_rollback(error, &shell_changes)),
                };
                eprintln!("Removed the HowTo Tab integration for {kind}.");
                shell_changes.push(result);
            }
        }
        if let Err(error) = shell::purge_pending(paths) {
            return Err(with_shell_rollback(error, &shell_changes));
        }
        None
    } else {
        let explicit_shell = options
            .shell
            .as_deref()
            .map(str::parse::<shell::Kind>)
            .transpose()?;
        let candidate = if let Some(kind) = explicit_shell.or(previous_shell) {
            Some(kind)
        } else if options.yes {
            detect_optional_shell()?
        } else if interactive() {
            if prompt_yes_no(
                "Enable Tab to insert your last generated command at an empty prompt? [Y/n] ",
                true,
            )? {
                detect_optional_shell()?
            } else {
                None
            }
        } else {
            return Err(Error::Configuration(
                "non-interactive setup needs `--yes`, `--no-shell`, or `--shell <zsh|bash|fish>`"
                    .into(),
            ));
        };
        if let Some(kind) = candidate {
            let recorded = if previous_shell == Some(kind) {
                previous_startup_files
            } else {
                &[]
            };
            let retry_command = format!("howto setup --shell {kind}");
            match enable_shell_with_consent(paths, kind, recorded, &retry_command) {
                Ok(ShellEnableOutcome::Enabled(result)) => {
                    let changed = result.changed;
                    let startup_files = result.startup_files.clone();
                    let backup_files = result.backup_files.clone();
                    chosen_startup_files.clone_from(&startup_files);
                    shell_changes.push(result);
                    if let Some(previous_kind) = previous_shell.filter(|previous| *previous != kind)
                    {
                        match shell::disable_recorded(paths, previous_kind, previous_startup_files)
                        {
                            Ok(disabled) => {
                                shell_changes.push(disabled);
                                eprintln!(
                                    "Disabled the previous Tab integration for {previous_kind}."
                                );
                            }
                            Err(error) => {
                                return Err(with_shell_rollback(error, &shell_changes));
                            }
                        }
                    }
                    if changed {
                        eprintln!("Enabled context-aware Tab integration for {kind}.");
                        for startup_file in &startup_files {
                            eprintln!("  Startup file: {}", startup_file.display());
                        }
                        for backup in &backup_files {
                            eprintln!("Previous startup file backed up to {}.", backup.display());
                        }
                    } else {
                        eprintln!("Tab integration is already configured for {kind}.");
                    }
                    Some(kind)
                }
                Ok(ShellEnableOutcome::Declined)
                    if explicit_shell.is_none() && previous_shell.is_none() =>
                {
                    eprintln!("Tab integration was skipped at your request.");
                    None
                }
                Ok(ShellEnableOutcome::Declined) if previous_shell.is_some() => {
                    return Err(Error::Configuration(
                        "the existing Tab integration was left unchanged because symlink approval was declined"
                            .into(),
                    ));
                }
                Ok(ShellEnableOutcome::Declined) => {
                    return Err(Error::Configuration(format!(
                        "Tab integration for {kind} was not enabled because symlink approval was declined; setup was not completed"
                    )));
                }
                Ok(ShellEnableOutcome::ApprovalRequired(message))
                    if explicit_shell.is_none() && previous_shell.is_none() =>
                {
                    eprintln!("Tab integration was skipped: {message}");
                    None
                }
                Ok(ShellEnableOutcome::ApprovalRequired(message)) => {
                    return Err(Error::Configuration(message));
                }
                Err(Error::Dependency(message))
                    if explicit_shell.is_none() && previous_shell.is_none() =>
                {
                    eprintln!("Tab integration was skipped: {message}");
                    None
                }
                Err(error) => return Err(error),
            }
        } else {
            None
        }
    };

    let mut new_receipt = setup::Receipt::new(None);
    new_receipt.set_shell(chosen_shell.map(shell::Kind::name), chosen_startup_files);
    if let Err(error) = setup::save(paths, &new_receipt) {
        return Err(with_shell_rollback(error, &shell_changes));
    }
    eprintln!("HowTo setup is complete.");
    if chosen_shell.is_some() && env::var_os(shell::SESSION_ENV).is_none() {
        eprintln!("Open a new terminal to activate the Tab shortcut.");
    }
    Ok(0)
}

fn with_shell_rollback(error: Error, changes: &[shell::EnableResult]) -> Error {
    let mut failures = Vec::new();
    for change in changes.iter().rev() {
        if let Err(rollback) = change.rollback() {
            failures.push(rollback.to_string());
        }
    }
    if failures.is_empty() {
        error
    } else {
        Error::Configuration(format!(
            "{error}; restoring the previous shell integration also failed: {}",
            failures.join("; ")
        ))
    }
}

fn prompt_yes_no(prompt: &str, default_yes: bool) -> Result<bool> {
    if !interactive() {
        return Err(Error::Execution(
            "this confirmation requires an interactive terminal".into(),
        ));
    }
    eprint!("{prompt}");
    io::stderr().flush()?;
    let mut answer = String::new();
    if io::stdin().read_line(&mut answer)? == 0 {
        return Ok(false);
    }
    let answer = answer.trim().to_ascii_lowercase();
    Ok(if answer.is_empty() {
        default_yes
    } else {
        matches!(answer.as_str(), "y" | "yes")
    })
}

fn detect_optional_shell() -> Result<Option<shell::Kind>> {
    match shell::detect() {
        Ok(kind) => Ok(Some(kind)),
        Err(Error::Dependency(message)) => {
            eprintln!("Tab integration was skipped: {message}");
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

enum ShellEnableOutcome {
    Enabled(shell::EnableResult),
    Declined,
    ApprovalRequired(String),
}

fn enable_shell_with_consent(
    paths: &Paths,
    kind: shell::Kind,
    recorded_startup_files: &[PathBuf],
    retry_command: &str,
) -> Result<ShellEnableOutcome> {
    let approvals = shell::startup_symlink_requests(paths, kind, recorded_startup_files)?;
    if approvals.is_empty() {
        return shell::enable_recorded(paths, kind, recorded_startup_files)
            .map(ShellEnableOutcome::Enabled);
    }

    if !interactive() {
        let links = approvals
            .iter()
            .map(|approval| {
                format!(
                    "{} -> {}",
                    approval.link_path().display(),
                    approval.target_path().display()
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        return Ok(ShellEnableOutcome::ApprovalRequired(format!(
            "shell startup symlink approval requires an interactive terminal ({links}); rerun `{retry_command}` to review the exact managed block and approve it, or leave integration disabled"
        )));
    }

    for approval in &approvals {
        eprintln!("HowTo found a symbolic link in the shell startup path:");
        eprintln!("  Startup path: {}", approval.link_path().display());
        eprintln!("  Resolved target: {}", approval.target_path().display());
        eprintln!("HowTo would write this exact block to the resolved target:");
        eprintln!("----- BEGIN HOWTO MANAGED BLOCK -----");
        eprint!("{}", approval.managed_block());
        if !approval.managed_block().ends_with('\n') {
            eprintln!();
        }
        eprintln!("----- END HOWTO MANAGED BLOCK -----");

        if !prompt_yes_no(
            "Allow HowTo to follow this symbolic link and update the resolved target? [y/N] ",
            false,
        )? {
            return Ok(ShellEnableOutcome::Declined);
        }
    }

    shell::enable_recorded_approved(paths, kind, recorded_startup_files, &approvals)
        .map(ShellEnableOutcome::Enabled)
}

fn interactive() -> bool {
    io::stdin().is_terminal() && io::stderr().is_terminal()
}

fn ensure_model(config: &Config, paths: &Paths) -> Result<ModelLocation> {
    if let Some(location) = model::resolve(config, paths)? {
        return Ok(location);
    }
    Err(Error::Model(format!(
        "the local model is unavailable; run `howto setup` to repair it (downloads {:.0} MiB if needed)",
        DEFAULT_MODEL.size as f64 / 1_048_576.0
    )))
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

fn run_shell(command: ShellCommand) -> Result<i32> {
    match command {
        ShellCommand::Init { shell: name } => {
            let kind = name.parse::<shell::Kind>()?;
            print!("{}", kind.adapter());
            Ok(0)
        }
        ShellCommand::Take => {
            runtime::refuse_elevated()?;
            let paths = Paths::discover()?;
            let _lock = setup::Lock::acquire(&paths)?;
            let Some(receipt) = setup::load(&paths)? else {
                return Ok(1);
            };
            let configured = receipt
                .shell
                .as_deref()
                .map(str::parse::<shell::Kind>)
                .transpose()?;
            if configured.is_none() || shell::session_kind()? != configured {
                return Ok(1);
            }
            if let Some(command) = shell::take_pending(&paths)? {
                println!("{command}");
                Ok(0)
            } else {
                Ok(1)
            }
        }
        ShellCommand::Enable { shell: name } => {
            runtime::refuse_elevated()?;
            let paths = Paths::discover()?;
            paths.create()?;
            let _lock = setup::Lock::acquire(&paths)?;
            let Some(mut receipt) = setup::load(&paths)? else {
                return Err(Error::Configuration(
                    "run `howto setup` before enabling shell integration".into(),
                ));
            };
            let kind = name
                .as_deref()
                .map(str::parse::<shell::Kind>)
                .transpose()?
                .unwrap_or(shell::detect()?);
            let previous = receipt
                .shell
                .as_deref()
                .map(str::parse::<shell::Kind>)
                .transpose()?;
            let previous_startup_files = receipt.shell_startup_files.clone();
            let recorded = if previous == Some(kind) {
                previous_startup_files.as_slice()
            } else {
                &[]
            };
            let retry_command = format!("howto shell enable --shell {kind}");
            let result = match enable_shell_with_consent(&paths, kind, recorded, &retry_command)? {
                ShellEnableOutcome::Enabled(result) => result,
                ShellEnableOutcome::Declined => {
                    eprintln!("Tab integration was not enabled; shell files were left unchanged.");
                    return Ok(1);
                }
                ShellEnableOutcome::ApprovalRequired(message) => {
                    return Err(Error::Configuration(message));
                }
            };
            let changed = result.changed;
            let startup_files = result.startup_files.clone();
            let backup_files = result.backup_files.clone();
            let mut shell_changes = vec![result];
            if let Some(previous) = previous.filter(|previous| *previous != kind) {
                match shell::disable_recorded(&paths, previous, &previous_startup_files) {
                    Ok(disabled) => {
                        shell_changes.push(disabled);
                        println!("Disabled the previous Tab integration for {previous}.");
                    }
                    Err(error) => {
                        return Err(with_shell_rollback(error, &shell_changes));
                    }
                }
            }
            receipt.set_shell(Some(kind.name()), startup_files.clone());
            if let Err(error) = setup::save(&paths, &receipt) {
                return Err(with_shell_rollback(error, &shell_changes));
            }
            println!(
                "Tab integration {} for {kind}.",
                if changed {
                    "enabled"
                } else {
                    "already enabled"
                }
            );
            for startup_file in &startup_files {
                println!("Startup file: {}", startup_file.display());
            }
            for backup in &backup_files {
                println!("Backup: {}", backup.display());
            }
            if env::var_os(shell::SESSION_ENV).is_none() {
                println!("Open a new terminal to activate it.");
            }
            Ok(0)
        }
        ShellCommand::Disable { shell: name } => {
            runtime::refuse_elevated()?;
            let paths = Paths::discover()?;
            paths.create()?;
            let _lock = setup::Lock::acquire(&paths)?;
            let mut receipt = setup::load(&paths)?;
            let kind = name
                .as_deref()
                .map(str::parse::<shell::Kind>)
                .transpose()?
                .or_else(|| {
                    receipt
                        .as_ref()
                        .and_then(|state| state.shell.as_deref())
                        .and_then(|value| value.parse().ok())
                })
                .unwrap_or(shell::detect()?);
            let recorded_startup_files = receipt
                .as_ref()
                .filter(|state| state.shell.as_deref() == Some(kind.name()))
                .map(|state| state.shell_startup_files.as_slice())
                .unwrap_or_default();
            let result = shell::disable_recorded(&paths, kind, recorded_startup_files)?;
            let changed = result.changed;
            let backup_files = result.backup_files.clone();
            if let Some(state) = &mut receipt {
                if state.shell.as_deref() == Some(kind.name()) {
                    state.set_shell(None, Vec::new());
                    if let Err(error) = setup::save(&paths, state) {
                        return Err(with_shell_rollback(error, std::slice::from_ref(&result)));
                    }
                }
            }
            println!(
                "Tab integration {} for {kind}.",
                if changed {
                    "disabled"
                } else {
                    "was not enabled"
                }
            );
            for backup in &backup_files {
                println!("Backup: {}", backup.display());
            }
            Ok(0)
        }
        ShellCommand::Status { json } => {
            let paths = Paths::discover()?;
            let receipt = setup::load(&paths)?;
            let configured_shell = receipt
                .as_ref()
                .and_then(|state| state.shell.as_deref())
                .map(str::parse::<shell::Kind>)
                .transpose()?;
            let enabled = configured_shell.is_some_and(|kind| {
                shell::is_enabled_at(
                    &paths,
                    kind,
                    receipt
                        .as_ref()
                        .map(|state| state.shell_startup_files.as_slice())
                        .unwrap_or_default(),
                )
            });
            let active = enabled
                && shell::session_kind()
                    .ok()
                    .flatten()
                    .is_some_and(|kind| Some(kind) == configured_shell);
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "setup_complete": receipt.is_some(),
                        "shell": configured_shell.map(shell::Kind::name),
                        "configured": enabled,
                        "active_in_this_shell": active,
                    }))?
                );
            } else {
                println!(
                    "Tab integration: {}",
                    if enabled {
                        "configured"
                    } else {
                        "not configured"
                    }
                );
                if let Some(kind) = configured_shell {
                    println!("Shell: {kind}");
                }
                println!(
                    "Current shell: {}",
                    if active { "active" } else { "not active" }
                );
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
    let setup_complete = setup::load(&paths)?.is_some();
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
    let provider_ready = provider_healthy.unwrap_or_else(|| {
        model.is_some() && runtime_usable == Some(true) && model_verified != Some(false)
    });
    let ready = setup_complete && provider_ready;
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "ready": ready,
                "setup_complete": setup_complete,
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
        println!(
            "Setup:   {}",
            if setup_complete {
                "complete"
            } else {
                "incomplete"
            }
        );
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
        "show_tab_hint" => Ok(config.show_tab_hint.to_string()),
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
        "show_tab_hint" => {
            config.show_tab_hint = parse_or_default(value, defaults.show_tab_hint, key)?;
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
        "show_tab_hint",
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
        config.threads = 3;

        set_config_value(&mut config, "show_tab_hint", Some("false")).unwrap();
        assert!(!config.show_tab_hint);
        assert_eq!(config_value(&config, "show_tab_hint").unwrap(), "false");
        assert!(set_config_value(&mut config, "show_tab_hint", Some("sometimes")).is_err());
    }

    #[test]
    fn unset_restores_default() {
        let mut config = Config {
            model_id: "custom".into(),
            show_tab_hint: false,
            ..Config::default()
        };
        set_config_value(&mut config, "model_id", None).unwrap();
        set_config_value(&mut config, "show_tab_hint", None).unwrap();
        assert_eq!(config.model_id, Config::default().model_id);
        assert!(config.show_tab_hint);
    }

    #[test]
    fn empty_argument_request_is_rejected_before_setup() {
        assert!(read_prompt(vec!["   ".into()]).is_err());
    }
}
