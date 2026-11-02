use std::io::{Read, Write};

use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::error::{Error, Result};

pub const ENGINE_PROTOCOL_SCHEMA: u32 = 2;
const MAX_FRAME_BYTES: usize = 64 * 1_024;
const MAX_ENGINE_MESSAGE_BYTES: usize = 4 * 1_024;
/// Leaves enough room for JSON framing even when every output byte is escaped.
pub const MAX_GENERATED_CONTENT_BYTES: usize = 8 * 1_024;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EngineRequest {
    pub schema: u32,
    pub authorization: String,
    pub command: EngineCommand,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum EngineCommand {
    Health,
    Generate(GenerateRequest),
    Shutdown,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GenerateRequest {
    pub messages: Vec<ChatMessage>,
    pub max_tokens: usize,
    pub temperature: f32,
    pub top_p: Option<f32>,
    pub repeat_penalty: f32,
    pub repeat_last_n: usize,
    pub seed: u32,
    pub stop: Vec<String>,
    pub stop_token_ids: Vec<i32>,
    /// Absolute wall-clock expiry for this request. An absolute timestamp is
    /// required because requests can wait in the resident worker's socket
    /// queue before the single inference loop accepts them.
    pub expires_at_unix_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum EngineResponse {
    Healthy,
    Generated(GenerateResponse),
    Stopping,
    Failed(EngineFailure),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GenerateResponse {
    pub content: String,
    pub finish_reason: FinishReason,
    pub prompt_tokens: usize,
    pub generated_tokens: usize,
    pub elapsed_ms: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EngineFailure {
    pub kind: EngineFailureKind,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineFailureKind {
    InvalidRequest,
    Model,
    Busy,
    Deadline,
    Internal,
}

pub fn write_frame<T: Serialize>(writer: &mut impl Write, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(Error::Server(format!(
            "embedded engine message is too large ({} > {MAX_FRAME_BYTES} bytes)",
            bytes.len()
        )));
    }
    let length = u32::try_from(bytes.len())
        .map_err(|_| Error::Server("embedded engine message length overflowed".into()))?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

pub fn read_frame<T: DeserializeOwned>(reader: &mut impl Read) -> Result<T> {
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length)?;
    let length = usize::try_from(u32::from_be_bytes(length))
        .map_err(|_| Error::Server("embedded engine message length overflowed".into()))?;
    if length > MAX_FRAME_BYTES {
        return Err(Error::Server(format!(
            "embedded engine message is too large ({length} > {MAX_FRAME_BYTES} bytes)"
        )));
    }
    let mut bytes = vec![0_u8; length];
    reader.read_exact(&mut bytes)?;
    serde_json::from_slice(&bytes).map_err(|error| {
        Error::Server(format!(
            "embedded engine returned an invalid protocol message: {error}"
        ))
    })
}

impl EngineRequest {
    pub fn validate(&self) -> Result<()> {
        if self.schema != ENGINE_PROTOCOL_SCHEMA {
            return Err(Error::Server(format!(
                "unsupported embedded engine protocol schema {}",
                self.schema
            )));
        }
        if self.authorization.len() != 64
            || !self
                .authorization
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(Error::Server(
                "embedded engine authorization is malformed".into(),
            ));
        }
        if let EngineCommand::Generate(request) = &self.command {
            request.validate()?;
        }
        Ok(())
    }
}

impl GenerateRequest {
    fn validate(&self) -> Result<()> {
        if self.messages.is_empty() || self.messages.len() > 8 {
            return Err(Error::Server(
                "embedded engine requires between 1 and 8 chat messages".into(),
            ));
        }
        let message_bytes = self
            .messages
            .iter()
            .try_fold(0_usize, |total, message| {
                total.checked_add(message.content.len())
            })
            .ok_or_else(|| Error::Server("embedded engine message length overflowed".into()))?;
        if message_bytes == 0 || message_bytes > 16 * 1_024 {
            return Err(Error::Server(
                "embedded engine chat content must be between 1 and 16384 bytes".into(),
            ));
        }
        if !(1..=4_096).contains(&self.max_tokens) {
            return Err(Error::Server(
                "embedded engine max_tokens must be between 1 and 4096".into(),
            ));
        }
        if !self.temperature.is_finite() || !(0.0..=2.0).contains(&self.temperature) {
            return Err(Error::Server(
                "embedded engine temperature must be finite and between 0 and 2".into(),
            ));
        }
        if self
            .top_p
            .is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
        {
            return Err(Error::Server(
                "embedded engine top_p must be finite and between 0 and 1".into(),
            ));
        }
        if !self.repeat_penalty.is_finite()
            || self.repeat_penalty <= 0.0
            || self.repeat_penalty > 2.0
        {
            return Err(Error::Server(
                "embedded engine repeat_penalty must be finite, greater than 0, and at most 2"
                    .into(),
            ));
        }
        if self.repeat_last_n > 4_096 {
            return Err(Error::Server(
                "embedded engine repeat_last_n must not exceed 4096".into(),
            ));
        }
        if self.stop.len() > 16
            || self.stop.iter().any(|stop| {
                stop.is_empty()
                    || stop.len() > 256
                    || stop.chars().any(|character| character == '\0')
            })
        {
            return Err(Error::Server(
                "embedded engine stop sequences are invalid".into(),
            ));
        }
        if self.stop_token_ids.len() > 16 || self.stop_token_ids.iter().any(|token| *token < 0) {
            return Err(Error::Server(
                "embedded engine stop token IDs are invalid".into(),
            ));
        }
        if self
            .expires_at_unix_ms
            .is_some_and(|deadline| deadline == 0)
        {
            return Err(Error::Server(
                "embedded engine deadline must be greater than zero".into(),
            ));
        }
        Ok(())
    }
}

impl EngineResponse {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Generated(response) if response.content.len() > MAX_GENERATED_CONTENT_BYTES => {
                Err(Error::Server(
                    "embedded engine generation is unexpectedly large".into(),
                ))
            }
            Self::Failed(failure) if failure.message.len() > MAX_ENGINE_MESSAGE_BYTES => Err(
                Error::Server("embedded engine error is unexpectedly large".into()),
            ),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{
        read_frame, write_frame, ChatMessage, ChatRole, EngineCommand, EngineRequest,
        EngineResponse, FinishReason, GenerateRequest, GenerateResponse, ENGINE_PROTOCOL_SCHEMA,
        MAX_FRAME_BYTES, MAX_GENERATED_CONTENT_BYTES,
    };

    fn request() -> EngineRequest {
        EngineRequest {
            schema: ENGINE_PROTOCOL_SCHEMA,
            authorization: "a".repeat(64),
            command: EngineCommand::Generate(GenerateRequest {
                messages: vec![ChatMessage {
                    role: ChatRole::User,
                    content: "list files".into(),
                }],
                max_tokens: 96,
                temperature: 0.0,
                top_p: None,
                repeat_penalty: 1.08,
                repeat_last_n: 64,
                seed: 42,
                stop: vec!["\n".into(), "<|im_end|>".into()],
                stop_token_ids: vec![248_044, 248_046],
                expires_at_unix_ms: Some(4_000_000_000_000),
            }),
        }
    }

    #[test]
    fn framed_protocol_round_trips() {
        let expected = request();
        expected.validate().unwrap();
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &expected).unwrap();
        let actual: EngineRequest = read_frame(&mut Cursor::new(bytes)).unwrap();
        actual.validate().unwrap();
        assert_eq!(actual.schema, expected.schema);
        assert_eq!(actual.authorization, expected.authorization);
        assert_eq!(actual.command, expected.command);
    }

    #[test]
    fn oversized_and_truncated_frames_fail_closed() {
        let oversized = u32::try_from(MAX_FRAME_BYTES + 1).unwrap().to_be_bytes();
        assert!(read_frame::<EngineResponse>(&mut Cursor::new(oversized)).is_err());

        let mut truncated = 12_u32.to_be_bytes().to_vec();
        truncated.extend_from_slice(b"{}");
        assert!(read_frame::<EngineResponse>(&mut Cursor::new(truncated)).is_err());
    }

    #[test]
    fn request_validation_rejects_bad_auth_and_generation_controls() {
        let mut value = request();
        value.authorization = "secret".into();
        assert!(value.validate().is_err());

        let mut value = request();
        let EngineCommand::Generate(generation) = &mut value.command else {
            panic!("expected generation request");
        };
        generation.temperature = f32::NAN;
        assert!(value.validate().is_err());

        let mut value = request();
        let EngineCommand::Generate(generation) = &mut value.command else {
            panic!("expected generation request");
        };
        generation.stop_token_ids = vec![-1];
        assert!(value.validate().is_err());
    }

    #[test]
    fn maximum_escaped_generation_fits_in_one_protocol_frame() {
        let response = EngineResponse::Generated(GenerateResponse {
            content: "\0".repeat(MAX_GENERATED_CONTENT_BYTES),
            finish_reason: FinishReason::Length,
            prompt_tokens: 2_048,
            generated_tokens: 4_096,
            elapsed_ms: 300_000,
        });
        response.validate().unwrap();
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &response).unwrap();
        assert!(bytes.len() <= MAX_FRAME_BYTES + 4);

        let mut oversized = response;
        let EngineResponse::Generated(generation) = &mut oversized else {
            unreachable!();
        };
        generation.content.push('x');
        assert!(oversized.validate().is_err());
    }
}
