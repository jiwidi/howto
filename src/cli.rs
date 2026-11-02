use std::ffi::OsString;

use crate::error::{Error, Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Invocation {
    Help,
    Version,
    Setup(SetupOptions),
    Query(QueryOptions),
    Doctor { json: bool, deep: bool },
    Model(ModelCommand),
    Server(ServerCommand),
    Config(ConfigCommand),
    Shell(ShellCommand),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetupOptions {
    pub yes: bool,
    pub no_shell: bool,
    pub shell: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryOptions {
    pub words: Vec<String>,
    pub execute: bool,
    pub copy: bool,
    pub quiet: bool,
    pub json: bool,
    pub timing: bool,
    pub count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelCommand {
    Status { json: bool, deep: bool },
    Install { yes: bool },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServerCommand {
    Status { json: bool },
    Stop { json: bool },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigCommand {
    List { json: bool },
    Path,
    Get { key: String },
    Set { key: String, value: String },
    Unset { key: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShellCommand {
    Init { shell: String },
    Enable { shell: Option<String> },
    Disable { shell: Option<String> },
    Status { json: bool },
    Take,
    AdvisorReady,
    Advise { status: u16 },
}

pub fn run_from_env() -> Result<i32> {
    let invocation = parse(std::env::args_os().skip(1))?;
    crate::app::run(invocation)
}

pub fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Invocation> {
    let arguments = arguments
        .into_iter()
        .map(|argument| {
            argument
                .into_string()
                .map_err(|_| Error::Usage("arguments must be valid UTF-8".into()))
        })
        .collect::<Result<Vec<_>>>()?;

    let Some(first) = arguments.first().map(String::as_str) else {
        return Ok(Invocation::Query(default_query(Vec::new())));
    };
    match first {
        "-h" | "--help" => exact_or_usage(&arguments, Invocation::Help),
        "-V" | "--version" => exact_or_usage(&arguments, Invocation::Version),
        "help" if arguments.len() == 1 => Ok(Invocation::Help),
        "version" if arguments.len() == 1 => Ok(Invocation::Version),
        "setup"
            if arguments.len() == 1
                || arguments
                    .get(1)
                    .is_some_and(|argument| argument.starts_with('-')) =>
        {
            parse_setup(&arguments[1..])
        }
        "doctor"
            if arguments.len() == 1
                || arguments
                    .get(1)
                    .is_some_and(|argument| argument.starts_with('-')) =>
        {
            parse_doctor(&arguments[1..])
        }
        "model"
            if arguments.len() == 1
                || arguments.get(1).is_some_and(|argument| {
                    matches!(argument.as_str(), "status" | "install") || argument.starts_with('-')
                }) =>
        {
            parse_model(&arguments[1..])
        }
        "server"
            if arguments.len() == 1
                || arguments.get(1).is_some_and(|argument| {
                    matches!(argument.as_str(), "status" | "stop") || argument.starts_with('-')
                }) =>
        {
            parse_server(&arguments[1..])
        }
        "config"
            if arguments.len() == 1
                || arguments.get(1).is_some_and(|argument| {
                    matches!(argument.as_str(), "list" | "path" | "get" | "set" | "unset")
                        || argument.starts_with('-')
                }) =>
        {
            parse_config(&arguments[1..])
        }
        "shell"
            if arguments.get(1).is_some_and(|argument| {
                matches!(
                    argument.as_str(),
                    "init" | "enable" | "disable" | "status" | "take" | "advisor-ready" | "advise"
                ) || argument.starts_with('-')
            }) =>
        {
            parse_shell(&arguments[1..])
        }
        _ => parse_query(&arguments),
    }
}

fn parse_setup(arguments: &[String]) -> Result<Invocation> {
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
    {
        return Ok(Invocation::Help);
    }
    let mut yes = false;
    let mut no_shell = false;
    let mut shell = None;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "-y" | "--yes" => yes = true,
            "--no-shell" => no_shell = true,
            "--shell" => {
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or_else(|| Error::Usage("--shell needs zsh, bash, or fish".into()))?;
                if !matches!(value.as_str(), "zsh" | "bash" | "fish") {
                    return Err(Error::Usage("--shell needs zsh, bash, or fish".into()));
                }
                shell = Some(value.clone());
            }
            argument => return Err(Error::Usage(format!("unknown setup option `{argument}`"))),
        }
        index += 1;
    }
    if no_shell && shell.is_some() {
        return Err(Error::Usage(
            "--no-shell and --shell cannot be used together".into(),
        ));
    }
    Ok(Invocation::Setup(SetupOptions {
        yes,
        no_shell,
        shell,
    }))
}

fn exact_or_usage(arguments: &[String], invocation: Invocation) -> Result<Invocation> {
    if arguments.len() == 1 {
        Ok(invocation)
    } else {
        Err(Error::Usage(format!(
            "`{}` does not accept arguments",
            arguments[0]
        )))
    }
}

fn parse_query(arguments: &[String]) -> Result<Invocation> {
    let mut options = default_query(Vec::new());
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--" => {
                options.words.extend_from_slice(&arguments[index + 1..]);
                break;
            }
            "-x" | "-e" | "--execute" => options.execute = true,
            "-c" | "--copy" => options.copy = true,
            "-q" | "--quiet" => options.quiet = true,
            "--json" => options.json = true,
            "--timing" => options.timing = true,
            "-n" | "--count" => {
                index += 1;
                let value = arguments.get(index).ok_or_else(|| {
                    Error::Usage("-n/--count needs a value between 1 and 8".into())
                })?;
                options.count = value
                    .parse::<usize>()
                    .map_err(|_| Error::Usage("-n/--count needs a value between 1 and 8".into()))?;
                if !(1..=8).contains(&options.count) {
                    return Err(Error::Usage("-n/--count must be between 1 and 8".into()));
                }
            }
            unknown if unknown.starts_with('-') => {
                return Err(Error::Usage(format!(
                    "unknown option `{unknown}`; put `--` before a request that begins with a dash"
                )));
            }
            _ => {
                // Options are intentionally front-only. Natural-language requests often
                // contain command flags (`find files with -name`), which must remain text.
                options.words.extend_from_slice(&arguments[index..]);
                break;
            }
        }
        index += 1;
    }
    if options.quiet && options.json {
        return Err(Error::Usage(
            "--quiet and --json cannot be used together".into(),
        ));
    }
    if options.execute && options.json {
        return Err(Error::Usage(
            "--execute and --json cannot be used together because command output would corrupt JSON"
                .into(),
        ));
    }
    if options.execute && options.quiet {
        return Err(Error::Usage(
            "--execute and --quiet cannot be used together".into(),
        ));
    }
    if options.quiet && options.count > 1 {
        return Err(Error::Usage(
            "--quiet requires --count 1 so no undisplayed alternative can be selected".into(),
        ));
    }
    Ok(Invocation::Query(options))
}

fn default_query(words: Vec<String>) -> QueryOptions {
    QueryOptions {
        words,
        execute: false,
        copy: false,
        quiet: false,
        json: false,
        timing: false,
        count: 1,
    }
}

fn parse_doctor(arguments: &[String]) -> Result<Invocation> {
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
    {
        return Ok(Invocation::Help);
    }
    let mut json = false;
    let mut deep = false;
    for argument in arguments {
        match argument.as_str() {
            "--json" => json = true,
            "--deep" => deep = true,
            _ => return Err(Error::Usage(format!("unknown doctor option `{argument}`"))),
        }
    }
    Ok(Invocation::Doctor { json, deep })
}

fn parse_model(arguments: &[String]) -> Result<Invocation> {
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
    {
        return Ok(Invocation::Help);
    }
    let (action, rest) = match arguments.first().map(String::as_str) {
        None => ("status", &[][..]),
        Some(option) if option.starts_with('-') => ("status", arguments),
        Some(action) => (action, &arguments[1..]),
    };
    match action {
        "status" => {
            let mut json = false;
            let mut deep = false;
            for argument in rest {
                match argument.as_str() {
                    "--json" => json = true,
                    "--deep" => deep = true,
                    _ => return Err(Error::Usage(format!("unknown model option `{argument}`"))),
                }
            }
            Ok(Invocation::Model(ModelCommand::Status { json, deep }))
        }
        "install" => {
            let mut yes = false;
            for argument in rest {
                match argument.as_str() {
                    "-y" | "--yes" => yes = true,
                    _ => return Err(Error::Usage(format!("unknown model option `{argument}`"))),
                }
            }
            Ok(Invocation::Model(ModelCommand::Install { yes }))
        }
        _ => Err(Error::Usage(format!(
            "unknown model command `{action}` (expected status or install)"
        ))),
    }
}

fn parse_server(arguments: &[String]) -> Result<Invocation> {
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
    {
        return Ok(Invocation::Help);
    }
    let (action, rest) = match arguments.first().map(String::as_str) {
        None => ("status", &[][..]),
        Some(option) if option.starts_with('-') => ("status", arguments),
        Some(action) => (action, &arguments[1..]),
    };
    let mut json = false;
    for argument in rest {
        if argument == "--json" {
            json = true;
        } else {
            return Err(Error::Usage(format!("unknown server option `{argument}`")));
        }
    }
    match action {
        "status" => Ok(Invocation::Server(ServerCommand::Status { json })),
        "stop" => Ok(Invocation::Server(ServerCommand::Stop { json })),
        _ => Err(Error::Usage(format!(
            "unknown server command `{action}` (expected status or stop)"
        ))),
    }
}

fn parse_config(arguments: &[String]) -> Result<Invocation> {
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
    {
        return Ok(Invocation::Help);
    }
    match arguments {
        [] => Ok(Invocation::Config(ConfigCommand::List { json: false })),
        [flag] if flag == "--json" => Ok(Invocation::Config(ConfigCommand::List { json: true })),
        [action] if action == "list" => Ok(Invocation::Config(ConfigCommand::List { json: false })),
        [action, flag] if action == "list" && flag == "--json" => {
            Ok(Invocation::Config(ConfigCommand::List { json: true }))
        }
        [action] if action == "path" => Ok(Invocation::Config(ConfigCommand::Path)),
        [action, key] if action == "get" => {
            Ok(Invocation::Config(ConfigCommand::Get { key: key.clone() }))
        }
        [action, key, value] if action == "set" => Ok(Invocation::Config(ConfigCommand::Set {
            key: key.clone(),
            value: value.clone(),
        })),
        [action, key] if action == "unset" => Ok(Invocation::Config(ConfigCommand::Unset {
            key: key.clone(),
        })),
        _ => Err(Error::Usage(
            "usage: howto config [list [--json]|path|get KEY|set KEY VALUE|unset KEY]".into(),
        )),
    }
}

fn parse_shell(arguments: &[String]) -> Result<Invocation> {
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
    {
        return Ok(Invocation::Help);
    }
    let Some(action) = arguments.first().map(String::as_str) else {
        return Err(Error::Usage(
            "usage: howto shell <init|enable|disable|status>".into(),
        ));
    };
    let rest = &arguments[1..];
    match action {
        "init" => match rest {
            [shell] if matches!(shell.as_str(), "zsh" | "bash" | "fish") => {
                Ok(Invocation::Shell(ShellCommand::Init {
                    shell: shell.clone(),
                }))
            }
            _ => Err(Error::Usage(
                "usage: howto shell init <zsh|bash|fish>".into(),
            )),
        },
        "enable" | "disable" => {
            let shell = match rest {
                [] => None,
                [flag, shell]
                    if flag == "--shell" && matches!(shell.as_str(), "zsh" | "bash" | "fish") =>
                {
                    Some(shell.clone())
                }
                _ => {
                    return Err(Error::Usage(format!(
                        "usage: howto shell {action} [--shell <zsh|bash|fish>]"
                    )))
                }
            };
            if action == "enable" {
                Ok(Invocation::Shell(ShellCommand::Enable { shell }))
            } else {
                Ok(Invocation::Shell(ShellCommand::Disable { shell }))
            }
        }
        "status" => match rest {
            [] => Ok(Invocation::Shell(ShellCommand::Status { json: false })),
            [flag] if flag == "--json" => {
                Ok(Invocation::Shell(ShellCommand::Status { json: true }))
            }
            _ => Err(Error::Usage("usage: howto shell status [--json]".into())),
        },
        "take" if rest.is_empty() => Ok(Invocation::Shell(ShellCommand::Take)),
        "advisor-ready" if rest.is_empty() => Ok(Invocation::Shell(ShellCommand::AdvisorReady)),
        "advise" => match rest {
            [flag, status] if flag == "--status" => {
                let status = status.parse::<u16>().map_err(|_| {
                    Error::Usage("usage: howto shell advise --status <0-65535>".into())
                })?;
                Ok(Invocation::Shell(ShellCommand::Advise { status }))
            }
            _ => Err(Error::Usage(
                "usage: howto shell advise --status <0-65535>".into(),
            )),
        },
        _ => Err(Error::Usage(format!(
            "unknown shell command `{action}` (expected init, enable, disable, or status)"
        ))),
    }
}

pub fn print_help() {
    println!(
        "HowTo {version}\n\
         Turn plain English into a shell command, locally.\n\n\
         USAGE:\n  howto [OPTIONS] <REQUEST...>\n  howto <COMMAND>\n\n\
         EXAMPLES:\n  howto \"free port 8080\"\n  howto find files larger than 1GB\n  howto -x show my current IP address\n\n\
         COMMANDS:\n  setup [OPTIONS]              Download the model and configure HowTo\n  doctor                       Check setup, the model, and runtime\n  model status                 Inspect the local model\n  model install                Download and verify the local model\n  server status                Inspect the resident model server\n  server stop                  Stop the resident model server\n  shell status                 Inspect shell integration\n  shell enable [--shell SHELL] Enable shell integration\n  shell disable [--shell SHELL]\n                                Disable shell integration\n  shell init <zsh|bash|fish>   Print an embedded shell adapter\n  config                       Read or change configuration\n  help                         Show this help\n\n\
         SETUP OPTIONS:\n  -y, --yes          Accept default-Yes prompts; never advisor/symlink consent\n      --no-shell     Complete setup and remove managed shell integration\n      --shell SHELL  Enable integration for zsh, bash, or fish\n\n\
         OPTIONS:\n  -x, -e, --execute   Execute after an explicit confirmation\n  -c, --copy          Copy the generated command\n  -q, --quiet         Print only a no-known-risk command\n  -n, --count N       Generate up to N alternatives (1-8)\n      --json          Emit machine-readable output\n      --timing        Show generation timing\n  -h, --help          Show this help\n  -V, --version       Show the version\n\n\
         Nothing executes by default. DANGER and UNKNOWN commands cannot execute.",
        version = crate::VERSION
    );
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{
        parse, ConfigCommand, Invocation, ModelCommand, QueryOptions, ServerCommand, SetupOptions,
        ShellCommand,
    };

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_natural_request() {
        assert_eq!(
            parse(args(&["free", "port", "8080"])).unwrap(),
            Invocation::Query(QueryOptions {
                words: vec!["free".into(), "port".into(), "8080".into()],
                execute: false,
                copy: false,
                quiet: false,
                json: false,
                timing: false,
                count: 1,
            })
        );
    }

    #[test]
    fn flags_are_front_only() {
        let Invocation::Query(query) = parse(args(&["find", "files", "with", "-n"])).unwrap()
        else {
            panic!("expected query");
        };
        assert_eq!(query.words.last().unwrap(), "-n");
    }

    #[test]
    fn double_dash_allows_hyphen_prompt() {
        let Invocation::Query(query) = parse(args(&["--", "--help", "in", "tar"])).unwrap() else {
            panic!("expected query");
        };
        assert_eq!(query.words[0], "--help");
    }

    #[test]
    fn parses_config_set() {
        assert_eq!(
            parse(args(&["config", "set", "threads", "3"])).unwrap(),
            Invocation::Config(ConfigCommand::Set {
                key: "threads".into(),
                value: "3".into(),
            })
        );
    }

    #[test]
    fn parses_setup_and_preserves_natural_setup_requests() {
        assert_eq!(
            parse(args(&["setup"])).unwrap(),
            Invocation::Setup(SetupOptions {
                yes: false,
                no_shell: false,
                shell: None,
            })
        );
        assert_eq!(
            parse(args(&["setup", "--yes", "--shell", "fish"])).unwrap(),
            Invocation::Setup(SetupOptions {
                yes: true,
                no_shell: false,
                shell: Some("fish".into()),
            })
        );
        let Invocation::Query(query) = parse(args(&["setup", "a", "venv"])).unwrap() else {
            panic!("expected a natural-language query");
        };
        assert_eq!(query.words, ["setup", "a", "venv"]);
    }

    #[test]
    fn parses_shell_management() {
        assert_eq!(
            parse(args(&["shell", "init", "zsh"])).unwrap(),
            Invocation::Shell(ShellCommand::Init {
                shell: "zsh".into()
            })
        );
        assert_eq!(
            parse(args(&["shell", "enable", "--shell", "bash"])).unwrap(),
            Invocation::Shell(ShellCommand::Enable {
                shell: Some("bash".into())
            })
        );
        let Invocation::Query(query) = parse(args(&["shell", "into", "a", "container"])).unwrap()
        else {
            panic!("expected a natural-language query");
        };
        assert_eq!(query.words[0], "shell");
    }

    #[test]
    fn parses_internal_failed_command_advisor() {
        assert_eq!(
            parse(args(&["shell", "advisor-ready"])).unwrap(),
            Invocation::Shell(ShellCommand::AdvisorReady)
        );
        assert!(parse(args(&["shell", "advisor-ready", "unexpected"])).is_err());
        assert_eq!(
            parse(args(&["shell", "advise", "--status", "127"])).unwrap(),
            Invocation::Shell(ShellCommand::Advise { status: 127 })
        );
        assert!(parse(args(&["shell", "advise", "127"])).is_err());
        assert!(parse(args(&["shell", "advise", "--status", "nope"])).is_err());
    }

    #[test]
    fn rejects_output_and_execution_mode_conflicts() {
        assert!(parse(args(&["--json", "--execute", "list", "files"])).is_err());
        assert!(parse(args(&["--quiet", "--execute", "list", "files"])).is_err());
        assert!(parse(args(&["--quiet", "-n", "2", "list", "files"])).is_err());
    }

    #[test]
    fn reserved_words_with_natural_followups_are_queries() {
        for words in [
            &["help", "me", "find", "large", "files"][..],
            &["doctor", "this", "shell", "script"],
            &["model", "the", "current", "directory"],
            &["server", "a", "local", "website"],
            &["config", "my", "git", "identity"],
            &["setup", "a", "python", "venv"],
            &["shell", "into", "a", "container"],
            &["version", "these", "files"],
        ] {
            let Invocation::Query(query) = parse(args(words)).unwrap() else {
                panic!("expected natural-language query for {words:?}");
            };
            assert_eq!(
                query.words.iter().map(String::as_str).collect::<Vec<_>>(),
                words
            );
        }
    }

    #[test]
    fn status_options_work_without_the_explicit_status_word() {
        assert_eq!(
            parse(args(&["model", "--json"])).unwrap(),
            Invocation::Model(ModelCommand::Status {
                json: true,
                deep: false,
            })
        );
        assert_eq!(
            parse(args(&["server", "--json"])).unwrap(),
            Invocation::Server(ServerCommand::Status { json: true })
        );
        assert_eq!(
            parse(args(&["config", "--json"])).unwrap(),
            Invocation::Config(ConfigCommand::List { json: true })
        );
        assert_eq!(
            parse(args(&["model", "install", "--help"])).unwrap(),
            Invocation::Help
        );
    }
}
