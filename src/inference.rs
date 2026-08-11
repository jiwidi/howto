use std::io::Read;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::extract;
use crate::platform::Platform;
use crate::runtime::Connection;

const MAX_RESPONSE_BYTES: u64 = 16 * 1_024 * 1_024;
const MAX_SERVER_ERROR_BYTES: usize = 2_048;
const STOP: [&str; 3] = ["\n", "<|im_end|>", "```"];

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Generation {
    pub commands: Vec<String>,
    pub elapsed_ms: u128,
}

pub struct Generator<'a> {
    connection: &'a Connection,
    config: &'a Config,
    platform: Platform,
}

impl<'a> Generator<'a> {
    #[must_use]
    pub const fn new(connection: &'a Connection, config: &'a Config, platform: Platform) -> Self {
        Self {
            connection,
            config,
            platform,
        }
    }

    pub fn generate(&self, prompt: &str, count: usize) -> Result<Generation> {
        if prompt.trim().is_empty() {
            return Err(Error::Usage("tell HowTo what you want to do".into()));
        }
        if prompt.len() > 8_192 {
            return Err(Error::Usage(
                "request is too long (maximum 8192 bytes)".into(),
            ));
        }
        let count = count.clamp(1, 8);
        let started = Instant::now();
        let mut raw = self.query(prompt, false, 0.0)?;
        let first = raw
            .pop()
            .ok_or_else(|| Error::InvalidResponse("the model returned no choices".into()));
        let first = match first
            .and_then(|choice| extract::command(&choice.content, choice.finish_reason.as_deref()))
        {
            Ok(command) => command,
            Err(_) => {
                let retry = self.query(prompt, true, 0.0)?;
                let choice = retry.into_iter().next().ok_or_else(|| {
                    Error::InvalidResponse("the model returned no choices".into())
                })?;
                extract::command(&choice.content, choice.finish_reason.as_deref())?
            }
        };
        let mut commands = vec![first];

        // llama-server intentionally supports one chat-completion choice per request.
        // Sample alternatives independently instead of relying on OpenAI's multi-choice `n`.
        for _ in 1..count {
            if let Ok(mut alternatives) = self.query(prompt, false, 0.6) {
                if let Some(choice) = alternatives.pop() {
                    if let Ok(command) =
                        extract::command(&choice.content, choice.finish_reason.as_deref())
                    {
                        if !commands.contains(&command) {
                            commands.push(command);
                        }
                    }
                }
            }
        }

        Ok(Generation {
            commands,
            elapsed_ms: started.elapsed().as_millis(),
        })
    }

    fn query(
        &self,
        prompt: &str,
        strict_retry: bool,
        temperature: f32,
    ) -> Result<Vec<ExtractedChoice>> {
        let system_prompt = self.platform.system_prompt(strict_retry);
        let body = CompletionRequest {
            model: &self.config.model_id,
            messages: [
                Message {
                    role: "system",
                    content: &system_prompt,
                },
                Message {
                    role: "user",
                    content: prompt,
                },
            ],
            max_tokens: self.config.max_tokens,
            temperature,
            top_p: (temperature > 0.0).then_some(0.95),
            n: 1,
            repeat_penalty: 1.08,
            repeat_last_n: 64,
            stop: &STOP,
            stream: false,
        };
        let response = self
            .connection
            .post("v1/chat/completions")
            .timeout(Duration::from_secs(180))
            .json(&body)
            .send()
            .map_err(|error| Error::Network(format!("inference request failed: {error}")))?;
        decode_response(response)
    }
}

#[derive(Serialize)]
struct CompletionRequest<'a> {
    model: &'a str,
    messages: [Message<'a>; 2],
    max_tokens: usize,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    n: usize,
    repeat_penalty: f32,
    repeat_last_n: usize,
    stop: &'a [&'a str],
    stream: bool,
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'static str,
    content: &'a str,
}

#[derive(Debug, Deserialize)]
struct CompletionResponse {
    #[serde(default)]
    choices: Vec<Choice>,
    error: Option<ApiError>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ChoiceMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChoiceMessage {
    content: String,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    message: Option<String>,
}

fn decode_response(mut response: reqwest::blocking::Response) -> Result<Vec<ExtractedChoice>> {
    let status = response.status();
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| Error::Network(format!("could not read model response: {error}")))?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(Error::InvalidResponse(
            "the inference server returned an unexpectedly large response".into(),
        ));
    }
    let parsed: CompletionResponse = serde_json::from_slice(&bytes).map_err(|error| {
        Error::InvalidResponse(format!(
            "the inference server returned invalid JSON: {error}"
        ))
    })?;
    if !status.is_success() || parsed.error.is_some() {
        let message = parsed
            .error
            .and_then(|error| error.message)
            .unwrap_or_else(|| format!("HTTP {status}"));
        let message = terminal_safe_server_error(&message);
        return Err(Error::Server(format!(
            "the inference server rejected the request: {message}"
        )));
    }
    Ok(parsed
        .choices
        .into_iter()
        .map(|choice| ExtractedChoice {
            content: choice.message.content,
            finish_reason: choice.finish_reason,
        })
        .collect())
}

fn terminal_safe_server_error(message: &str) -> String {
    let mut output = String::with_capacity(message.len().min(MAX_SERVER_ERROR_BYTES));
    let mut truncated = false;
    for character in message.chars() {
        let unsafe_character = character.is_control()
            || matches!(
                character,
                '\u{061c}'
                    | '\u{200e}'
                    | '\u{200f}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2066}'..='\u{2069}'
            );
        let escaped = if unsafe_character {
            format!("\\u{{{:x}}}", u32::from(character))
        } else {
            character.to_string()
        };
        if output.len() + escaped.len() > MAX_SERVER_ERROR_BYTES {
            truncated = true;
            break;
        }
        output.push_str(&escaped);
    }
    if truncated {
        while output.len() + '…'.len_utf8() > MAX_SERVER_ERROR_BYTES {
            output.pop();
        }
        output.push('…');
    }
    if output.trim().is_empty() {
        "unspecified server error".into()
    } else {
        output
    }
}

struct ExtractedChoice {
    content: String,
    finish_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{
        terminal_safe_server_error, CompletionRequest, Message, MAX_SERVER_ERROR_BYTES, STOP,
    };

    #[test]
    fn request_has_generation_guards() {
        let request = CompletionRequest {
            model: "howto",
            messages: [
                Message {
                    role: "system",
                    content: "one line",
                },
                Message {
                    role: "user",
                    content: "list files",
                },
            ],
            max_tokens: 64,
            temperature: 0.0,
            top_p: None,
            n: 1,
            repeat_penalty: 1.08,
            repeat_last_n: 64,
            stop: &STOP,
            stream: false,
        };
        let value = serde_json::to_value(request).unwrap();
        assert_eq!(value["temperature"], 0.0);
        assert_eq!(value["n"], 1);
        assert_eq!(value["stop"][0], "\n");
        assert!(value.get("top_p").is_none());
    }

    #[test]
    fn provider_errors_are_terminal_safe_and_bounded() {
        let unsafe_message = format!("\u{1b}[31mboom\n\u{202e}{}", "x".repeat(8_192));
        let safe = terminal_safe_server_error(&unsafe_message);
        assert!(safe.len() <= MAX_SERVER_ERROR_BYTES);
        assert!(!safe.contains('\u{1b}'));
        assert!(!safe.contains('\n'));
        assert!(!safe.contains('\u{202e}'));
        assert!(safe.contains("\\u{1b}"));
        assert!(safe.ends_with('…'));
    }
}
