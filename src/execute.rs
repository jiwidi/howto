use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::error::{Error, Result};
use crate::platform::Platform;

pub fn confirm(prompt: &str, required_text: &str) -> Result<bool> {
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return Err(Error::Execution(
            "execution requires an interactive terminal for confirmation".into(),
        ));
    }
    eprint!("{prompt}");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(answer.trim() == required_text)
}

pub fn run(command: &str, shell: &Path) -> Result<i32> {
    if !shell.is_file() {
        return Err(Error::Execution(format!(
            "configured shell does not exist: {}",
            shell.display()
        )));
    }
    let mut process = Command::new(shell);
    sanitize_shell_environment(&mut process);
    match shell.file_name().and_then(|name| name.to_str()) {
        Some("zsh") => {
            process.args(["-f", "-c", command]);
        }
        Some("bash") => {
            process.args(["--noprofile", "--norc", "-c", command]);
        }
        Some("sh" | "dash") => {
            process.args(["-c", command]);
        }
        Some(name) => {
            return Err(Error::Configuration(format!(
                "unsupported execution shell `{name}`; use zsh, bash, sh, or dash"
            )));
        }
        None => {
            return Err(Error::Configuration(
                "configured shell path has no executable name".into(),
            ));
        }
    }
    let status = process
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|error| Error::Execution(format!("could not execute command: {error}")))?;
    Ok(status.code().unwrap_or(128 + status.signal().unwrap_or(1)))
}

fn sanitize_shell_environment(process: &mut Command) {
    for key in [
        "BASH_ENV",
        "ENV",
        "HOWTO_API_KEY",
        "BASHOPTS",
        "SHELLOPTS",
        "CDPATH",
        "GLOBIGNORE",
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "DYLD_INSERT_LIBRARIES",
        "DYLD_LIBRARY_PATH",
        "DYLD_FRAMEWORK_PATH",
        "DYLD_FORCE_FLAT_NAMESPACE",
        "PYTHONPATH",
        "PYTHONHOME",
        "PYTHONSTARTUP",
        "NODE_OPTIONS",
        "RUBYOPT",
        "RUBYLIB",
        "PERL5OPT",
        "PERL5LIB",
        "JAVA_TOOL_OPTIONS",
        "JDK_JAVA_OPTIONS",
        "_JAVA_OPTIONS",
        "LESSOPEN",
        "LESSCLOSE",
    ] {
        process.env_remove(key);
    }
    for key in std::env::vars_os().map(|(key, _)| key) {
        if is_exported_shell_function(&key) {
            process.env_remove(key);
        }
    }
}

fn is_exported_shell_function(key: &std::ffi::OsStr) -> bool {
    key.to_string_lossy().starts_with("BASH_FUNC_")
}

#[cfg(unix)]
trait ExitStatusSignal {
    fn signal(&self) -> Option<i32>;
}

#[cfg(unix)]
impl ExitStatusSignal for std::process::ExitStatus {
    fn signal(&self) -> Option<i32> {
        std::os::unix::process::ExitStatusExt::signal(self)
    }
}

pub fn copy_to_clipboard(contents: &str, platform: Platform) -> Result<()> {
    let candidates: &[(&str, &[&str])] = match platform {
        Platform::Macos => &[("/usr/bin/pbcopy", &[])],
        Platform::Linux => &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ],
    };
    let failures =
        match try_clipboard_candidates(candidates, program_exists, |program, arguments| {
            run_clipboard_helper(program, arguments, contents)
        }) {
            Ok(()) => return Ok(()),
            Err(failures) => failures,
        };
    if failures.is_empty() {
        let hint = if platform == Platform::Linux {
            "install wl-clipboard, xclip, or xsel"
        } else {
            "pbcopy is unavailable"
        };
        Err(Error::Dependency(format!(
            "could not copy to the clipboard; {hint}"
        )))
    } else {
        Err(Error::Execution(format!(
            "could not copy to the clipboard; helper attempts failed: {}; check the desktop clipboard session",
            failures.join("; ")
        )))
    }
}

fn try_clipboard_candidates<Available, Attempt>(
    candidates: &[(&str, &[&str])],
    mut available: Available,
    mut attempt: Attempt,
) -> std::result::Result<(), Vec<String>>
where
    Available: FnMut(&str) -> bool,
    Attempt: FnMut(&str, &[&str]) -> std::result::Result<(), String>,
{
    let mut failures = Vec::new();
    for (program, arguments) in candidates {
        if !available(program) {
            continue;
        }
        match attempt(program, arguments) {
            Ok(()) => return Ok(()),
            Err(error) => failures.push(format!("{program}: {error}")),
        }
    }
    Err(failures)
}

fn run_clipboard_helper(
    program: &str,
    arguments: &[&str],
    contents: &str,
) -> std::result::Result<(), String> {
    let mut process = Command::new(program);
    sanitize_shell_environment(&mut process);
    let mut child = process
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("could not start ({error})"))?;

    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("started without a writable stdin".into());
    };
    if let Err(error) = stdin.write_all(contents.as_bytes()) {
        drop(stdin);
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("could not receive clipboard input ({error})"));
    }
    drop(stdin);

    let status = child
        .wait()
        .map_err(|error| format!("could not wait for completion ({error})"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("exited with {status}"))
    }
}

fn program_exists(program: &str) -> bool {
    if program.contains('/') {
        return Path::new(program).is_file();
    }
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|directory| directory.join(program).is_file())
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::process::Command;

    use super::{
        is_exported_shell_function, program_exists, sanitize_shell_environment,
        try_clipboard_candidates,
    };

    #[test]
    fn finds_system_shell() {
        assert!(program_exists("/bin/sh"));
    }

    #[test]
    fn recognizes_exported_bash_functions() {
        assert!(is_exported_shell_function(OsStr::new("BASH_FUNC_rm%%")));
        assert!(!is_exported_shell_function(OsStr::new("PATH")));
    }

    #[test]
    fn removes_howto_provider_credential_from_executed_commands() {
        let mut command = Command::new("/bin/sh");
        command.env("HOWTO_API_KEY", "top-secret");
        sanitize_shell_environment(&mut command);
        assert!(command
            .get_envs()
            .any(|(key, value)| { key == OsStr::new("HOWTO_API_KEY") && value.is_none() }));
    }

    #[test]
    fn clipboard_selection_continues_after_helper_failures() {
        let candidates: &[(&str, &[&str])] = &[
            ("unavailable", &[]),
            ("broken-spawn", &[]),
            ("broken-write", &[]),
            ("working", &[]),
        ];
        let mut attempted = Vec::new();
        let result = try_clipboard_candidates(
            candidates,
            |program| program != "unavailable",
            |program, _| {
                attempted.push(program.to_owned());
                match program {
                    "working" => Ok(()),
                    _ => Err(format!("simulated {program} failure")),
                }
            },
        );

        assert!(result.is_ok());
        assert_eq!(attempted, ["broken-spawn", "broken-write", "working"]);
    }

    #[test]
    fn clipboard_selection_preserves_all_failure_reasons() {
        let candidates: &[(&str, &[&str])] = &[("wl-copy", &[]), ("xclip", &[])];
        let failures = try_clipboard_candidates(
            candidates,
            |_| true,
            |program, _| Err(format!("{program} failed")),
        )
        .unwrap_err();

        assert_eq!(failures, ["wl-copy: wl-copy failed", "xclip: xclip failed"]);
    }
}
