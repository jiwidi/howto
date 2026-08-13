use thiserror::Error;

const MAX_TERMINAL_ERROR_BYTES: usize = 4_096;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    Usage(String),
    #[error("{0}")]
    Configuration(String),
    #[error("{0}")]
    Dependency(String),
    #[error("{0}")]
    Model(String),
    #[error("{0}")]
    Server(String),
    #[error("{0}")]
    Network(String),
    #[error("{0}")]
    InvalidResponse(String),
    #[error("{0}")]
    Execution(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl Error {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_) => 2,
            Self::Configuration(_) | Self::Dependency(_) | Self::Model(_) => 3,
            Self::Server(_) | Self::Network(_) | Self::InvalidResponse(_) => 4,
            Self::Execution(_) | Self::Io(_) | Self::Json(_) => 1,
        }
    }
}

/// Makes a top-level diagnostic safe and bounded before it reaches a terminal.
///
/// Errors can contain provider text, environment-derived paths, or operating
/// system messages. Escaping controls and directional formatting here keeps
/// every CLI error path safe, including the automatic shell advisor path.
#[must_use]
pub fn terminal_safe_message(message: &str) -> String {
    let mut output = String::with_capacity(message.len().min(MAX_TERMINAL_ERROR_BYTES));
    let mut truncated = false;
    for character in message.chars() {
        let unsafe_character = character.is_control()
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
        if output.len() + escaped.len() > MAX_TERMINAL_ERROR_BYTES {
            truncated = true;
            break;
        }
        output.push_str(&escaped);
    }
    if truncated {
        while output.len() + '…'.len_utf8() > MAX_TERMINAL_ERROR_BYTES {
            output.pop();
        }
        output.push('…');
    }
    if output.trim().is_empty() {
        "unspecified error".into()
    } else {
        output
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::{terminal_safe_message, MAX_TERMINAL_ERROR_BYTES};

    #[test]
    fn top_level_error_messages_are_terminal_safe_and_bounded() {
        let message = format!("boom\n\u{1b}[31m\u{202e}{}", "x".repeat(8_192));
        let safe = terminal_safe_message(&message);
        assert!(safe.len() <= MAX_TERMINAL_ERROR_BYTES);
        assert!(!safe.contains('\n'));
        assert!(!safe.contains('\u{1b}'));
        assert!(!safe.contains('\u{202e}'));
        assert!(safe.contains("\\u{a}"));
        assert!(safe.contains("\\u{1b}"));
        assert!(safe.ends_with('…'));
        assert_eq!(terminal_safe_message("\n\t"), "\\u{a}\\u{9}");
    }
}
