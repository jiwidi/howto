use regex::Regex;

use crate::error::{Error, Result};

pub const MAX_COMMAND_BYTES: usize = 4_096;

pub fn command(raw: &str, finish_reason: Option<&str>) -> Result<String> {
    if finish_reason == Some("length") {
        return Err(Error::InvalidResponse(
            "the model output hit its token limit before completing a command".into(),
        ));
    }
    if raw.is_empty() {
        return Err(Error::InvalidResponse(
            "the model returned an empty response".into(),
        ));
    }
    reject_unsafe_controls(raw)?;

    let think = Regex::new(r"(?is)<think>.*?</think>").expect("valid think regex");
    let mut text = think.replace_all(raw, "").trim().to_owned();
    if let Some(position) = text.to_ascii_lowercase().find("<think>") {
        text.truncate(position);
        text = text.trim().to_owned();
    }
    if text.is_empty() {
        return Err(Error::InvalidResponse(
            "the model returned reasoning but no command".into(),
        ));
    }

    text = unwrap_fence(&text)?;
    text = strip_known_preamble(&text);
    text = strip_prompt_symbol(&text);
    text = strip_inline_backticks(&text);
    let text = text.trim();

    if text.is_empty() {
        return Err(Error::InvalidResponse(
            "the model returned no command".into(),
        ));
    }
    if text.len() > MAX_COMMAND_BYTES {
        return Err(Error::InvalidResponse(format!(
            "the generated command exceeds {MAX_COMMAND_BYTES} bytes"
        )));
    }
    if text.lines().count() != 1 || text.contains('\r') {
        return Err(Error::InvalidResponse(
            "the model returned multiple lines instead of one command".into(),
        ));
    }
    if text.contains("```") {
        return Err(Error::InvalidResponse(
            "the model returned malformed markdown instead of one command".into(),
        ));
    }
    Ok(text.to_owned())
}

fn reject_unsafe_controls(raw: &str) -> Result<()> {
    if raw.chars().any(|character| {
        character == '\0'
            || character == '\u{1b}'
            || character == '\u{7f}'
            || is_invisible_format(character)
    }) {
        return Err(Error::InvalidResponse(
            "the model output contained terminal control characters".into(),
        ));
    }
    if raw
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(Error::InvalidResponse(
            "the model output contained hidden control bytes".into(),
        ));
    }
    Ok(())
}

fn is_invisible_format(character: char) -> bool {
    matches!(
        character,
        '\u{061c}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{feff}'
    )
}

fn unwrap_fence(text: &str) -> Result<String> {
    if !text.starts_with("```") {
        return Ok(text.to_owned());
    }
    let Some(first_newline) = text.find('\n') else {
        return Err(Error::InvalidResponse(
            "the model returned an incomplete code fence".into(),
        ));
    };
    let header = &text[3..first_newline].trim().to_ascii_lowercase();
    if !header.is_empty() && !matches!(header.as_str(), "sh" | "bash" | "zsh" | "shell") {
        return Err(Error::InvalidResponse(format!(
            "the model returned an unexpected `{header}` code block"
        )));
    }
    let remainder = &text[first_newline + 1..];
    let Some(body) = remainder.strip_suffix("```") else {
        return Err(Error::InvalidResponse(
            "the model returned an incomplete code fence".into(),
        ));
    };
    if body.contains("```") {
        return Err(Error::InvalidResponse(
            "the model returned multiple code blocks".into(),
        ));
    }
    Ok(body.trim().to_owned())
}

fn strip_known_preamble(text: &str) -> String {
    let preamble = Regex::new(
        r"(?i)^(?:sure|certainly|here(?:'s| is)|you can|the command|this command|answer|command)\b[^\n]*?(?::|\n)\s*",
    )
    .expect("valid preamble regex");
    preamble.replace(text, "").into_owned()
}

fn strip_prompt_symbol(text: &str) -> String {
    Regex::new(r"^\$\s+")
        .expect("valid prompt regex")
        .replace(text, "")
        .into_owned()
}

fn strip_inline_backticks(text: &str) -> String {
    if text.len() > 1
        && text.starts_with('`')
        && text.ends_with('`')
        && !text[1..text.len() - 1].contains('`')
    {
        text[1..text.len() - 1].trim().to_owned()
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::command;

    #[test]
    fn accepts_plain_command() {
        assert_eq!(
            command("lsof -ti :8080", Some("stop")).unwrap(),
            "lsof -ti :8080"
        );
    }

    #[test]
    fn unwraps_one_fence_and_prompt() {
        assert_eq!(
            command("```zsh\n$ lsof -ti :8080\n```", None).unwrap(),
            "lsof -ti :8080"
        );
    }

    #[test]
    fn strips_reasoning_and_known_preamble() {
        assert_eq!(
            command("<think>hmm</think>\nCommand:\n`pwd`", None).unwrap(),
            "pwd"
        );
    }

    #[test]
    fn rejects_multiple_lines() {
        assert!(command("echo one\necho two", None).is_err());
    }

    #[test]
    fn rejects_ansi_and_truncation() {
        assert!(command("echo safe\u{1b}[8m; rm -rf /", None).is_err());
        assert!(command("echo safe \u{202e} /", None).is_err());
        assert!(command("echo safe \u{061c} /", None).is_err());
        assert!(command("echo", Some("length")).is_err());
    }

    #[test]
    fn does_not_activate_redirections_or_comments_as_prompt_text() {
        assert_eq!(command("> output.txt", None).unwrap(), "> output.txt");
        assert_eq!(command("# rm -rf /", None).unwrap(), "# rm -rf /");
    }
}
