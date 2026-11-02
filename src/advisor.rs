//! Conservative eligibility checks for the opt-in failed-command advisor.
//!
//! A shell hook runs after arbitrary interactive commands, so this boundary is
//! intentionally much narrower than the normal natural-language query path.
//! Only one literal external command is eligible. Everything that could hide
//! additional shell evaluation, include a secret, or represent an interrupt is
//! ignored without contacting the model.

use std::env;
use std::path::Path;

use tree_sitter::{Node, Parser};

pub const MAX_FAILED_COMMAND_BYTES: usize = 4_096;

/// Returns whether a failed interactive command may be sent to the managed
/// local model for a suggestion.
#[must_use]
pub fn is_eligible(command: &str, status: u16) -> bool {
    is_eligible_with(command, status, external_command_exists)
}

fn is_eligible_with(
    command: &str,
    status: u16,
    external_exists: impl Fn(&str, u16) -> bool,
) -> bool {
    if !(1..128).contains(&status)
        || command.is_empty()
        || command.len() > MAX_FAILED_COMMAND_BYTES
        || command.starts_with(char::is_whitespace)
        || command.chars().any(unsafe_character)
        || contains_secret_like_text(command)
    {
        return false;
    }

    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .is_err()
    {
        return false;
    }
    let Some(tree) = parser.parse(command, None) else {
        return false;
    };
    let root = tree.root_node();
    if root.has_error()
        || root.child_count() != 1
        || root.start_byte() != 0
        || root.end_byte() != command.len()
    {
        return false;
    }
    let Some(invocation) = root.child(0) else {
        return false;
    };
    if invocation.kind() != "command"
        || invocation.start_byte() != 0
        || invocation.end_byte() != command.len()
        || !contains_only_literal_nodes(invocation, command)
    {
        return false;
    }

    let Some(name_node) = invocation.child_by_field_name("name") else {
        return false;
    };
    let Some(word) = name_node.named_child(0) else {
        return false;
    };
    if word.kind() != "word" {
        return false;
    }
    let Ok(executable) = word.utf8_text(command.as_bytes()) else {
        return false;
    };
    if executable.is_empty()
        || executable.chars().any(shell_metacharacter)
        || is_shell_builtin_or_wrapper(executable)
    {
        return false;
    }

    // 127 is the shell's command-not-found status, so absence from PATH is the
    // expected failure and remains useful input. Every other status must name
    // an external file instead of a shell builtin, alias-only name, or function.
    status == 127 || external_exists(executable, status)
}

fn contains_only_literal_nodes(node: Node<'_>, source: &str) -> bool {
    let allowed = match node.kind() {
        "command" | "command_name" | "number" | "raw_string" | "string" | "string_content" => true,
        "word" => node
            .utf8_text(source.as_bytes())
            .is_ok_and(|word| !word.chars().any(shell_metacharacter)),
        _ => false,
    };
    if !allowed {
        return false;
    }
    let mut cursor = node.walk();
    let children_are_literal = node
        .named_children(&mut cursor)
        .all(|child| contains_only_literal_nodes(child, source));
    children_are_literal
}

fn shell_metacharacter(character: char) -> bool {
    matches!(
        character,
        '$' | '`' | '*' | '?' | '[' | ']' | '{' | '}' | '~' | '\\'
    )
}

fn unsafe_character(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{061c}'
                | '\u{200b}'..='\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                | '\u{feff}'
        )
}

fn contains_secret_like_text(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    if [
        "password",
        "passwd",
        "secret",
        "api_key",
        "api-key",
        "apikey",
        "access_key",
        "access-key",
        "private_key",
        "private-key",
        "authorization",
        "bearer ",
        "github_pat_",
        "ghp_",
        "glpat-",
        "xoxb-",
        "xoxp-",
        "sk_live_",
        "sk-proj-",
        "-----begin ",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return true;
    }
    if lower.split_ascii_whitespace().any(|word| {
        word == "token"
            || word.starts_with("--token=")
            || word.starts_with("token=")
            || word.starts_with("akia") && word.len() >= 20
    }) {
        return true;
    }
    if lower.split_ascii_whitespace().any(|word| {
        word.find("://").is_some_and(|scheme| {
            word[scheme + 3..]
                .split('/')
                .next()
                .is_some_and(|authority| authority.contains('@'))
        })
    }) {
        return true;
    }

    // Long uninterrupted encoded-looking values are commonly credentials or
    // private material. False negatives are costlier than skipped beta advice.
    command
        .split(|character: char| {
            character.is_whitespace() || matches!(character, '\'' | '"' | ':' | ',' | ';')
        })
        .any(looks_like_opaque_secret)
}

fn looks_like_opaque_secret(value: &str) -> bool {
    if value.len() < 32
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "+/_=-".contains(character))
    {
        return false;
    }
    let has_alpha = value
        .chars()
        .any(|character| character.is_ascii_alphabetic());
    let has_digit = value.chars().any(|character| character.is_ascii_digit());
    has_alpha && has_digit
}

fn is_shell_builtin_or_wrapper(executable: &str) -> bool {
    let name = Path::new(executable)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(executable)
        .to_ascii_lowercase();
    matches!(
        name.as_str(),
        "." | "["
            | "alias"
            | "arch"
            | "at"
            | "batch"
            | "bg"
            | "bind"
            | "break"
            | "builtin"
            | "busybox"
            | "caffeinate"
            | "caller"
            | "cd"
            | "chroot"
            | "chrt"
            | "command"
            | "compgen"
            | "complete"
            | "compopt"
            | "continue"
            | "declare"
            | "dirs"
            | "disown"
            | "doas"
            | "echo"
            | "enable"
            | "env"
            | "eval"
            | "exec"
            | "exit"
            | "export"
            | "false"
            | "fc"
            | "fg"
            | "flock"
            | "getopts"
            | "hash"
            | "help"
            | "history"
            | "howto"
            | "ionice"
            | "jobs"
            | "kill"
            | "konsole"
            | "let"
            | "local"
            | "logout"
            | "mapfile"
            | "nice"
            | "nohup"
            | "nsenter"
            | "parallel"
            | "printf"
            | "pushd"
            | "pwd"
            | "read"
            | "readarray"
            | "readonly"
            | "return"
            | "screen"
            | "script"
            | "set"
            | "setsid"
            | "shift"
            | "shopt"
            | "source"
            | "stdbuf"
            | "sudo"
            | "su"
            | "suspend"
            | "systemd-run"
            | "taskset"
            | "test"
            | "time"
            | "timeout"
            | "gtimeout"
            | "tmux"
            | "toybox"
            | "trap"
            | "true"
            | "type"
            | "typeset"
            | "ulimit"
            | "umask"
            | "unalias"
            | "unshare"
            | "unset"
            | "wait"
            | "watch"
            | "xargs"
            | "xterm"
            | "gnome-terminal"
            | "bash"
            | "dash"
            | "fish"
            | "ksh"
            | "sh"
            | "zsh"
    )
}

fn external_command_exists(executable: &str, status: u16) -> bool {
    let candidate_is_usable = |path: &Path| {
        let Ok(metadata) = path.metadata() else {
            return false;
        };
        if !metadata.is_file() {
            return false;
        }
        if status == 126 {
            return true;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            true
        }
    };

    let executable = Path::new(executable);
    if executable.components().count() > 1 || executable.is_absolute() {
        return candidate_is_usable(executable);
    }
    env::var_os("PATH").is_some_and(|path| {
        env::split_paths(&path).any(|directory| candidate_is_usable(&directory.join(executable)))
    })
}

#[cfg(test)]
mod tests {
    use super::is_eligible_with;

    fn eligible(command: &str, status: u16) -> bool {
        is_eligible_with(command, status, |_, _| true)
    }

    #[test]
    fn accepts_one_literal_external_failure_or_missing_command() {
        assert!(eligible("git sttaus", 2));
        assert!(eligible("missing-command --flag value", 127));
        assert!(eligible("curl 'https://example.com/health'", 22));
    }

    #[test]
    fn rejects_success_interrupts_and_oversized_input() {
        assert!(!eligible("git status", 0));
        assert!(!eligible("git status", 128));
        assert!(!eligible("git status", 130));
        assert!(!eligible(&format!("tool {}", "x".repeat(4_096)), 1));
    }

    #[test]
    fn rejects_nonliteral_or_compound_shell_syntax() {
        for command in [
            " git status",
            "git status\n",
            "git status && echo done",
            "git status | cat",
            "git status >out",
            "MODE=debug git status",
            "git $(printf status)",
            "git status &",
            "git st*",
            "git \\status",
            "git status # retry",
        ] {
            assert!(!eligible(command, 1), "unexpectedly eligible: {command:?}");
        }
    }

    #[test]
    fn rejects_builtins_privilege_and_launch_wrappers() {
        for command in [
            "cd missing",
            "sudo git status",
            "env git status",
            "command git status",
            "nohup git status",
            "bash -c 'git status'",
            "howto free port 8080",
        ] {
            assert!(!eligible(command, 1), "unexpectedly eligible: {command:?}");
        }
    }

    #[test]
    fn rejects_control_bidi_and_secret_like_text() {
        for command in [
            "git\tstatus",
            "tool \u{202e}value",
            "curl --token=abcd example.com",
            "curl https://user:pass@example.com",
            "tool AKIAIOSFODNN7EXAMPLE",
            "tool aaaaaaaaaaaaaaaa1111111111111111",
        ] {
            assert!(!eligible(command, 1), "unexpectedly eligible: {command:?}");
        }
    }

    #[test]
    fn non_missing_failures_require_a_resolved_external_command() {
        assert!(!is_eligible_with("unknown-tool arg", 1, |_, _| false));
        assert!(is_eligible_with("unknown-tool arg", 127, |_, _| false));
        assert!(is_eligible_with("known-tool arg", 2, |name, status| {
            name == "known-tool" && status == 2
        }));
    }
}
