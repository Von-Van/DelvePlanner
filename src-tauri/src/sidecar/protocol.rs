//! The wire format between the app and the Python agent: one JSON object per line on the child's
//! stdin and stdout, every message wrapped in the same small envelope. The Python twin of this file
//! is `agent-python/delveops_agent/protocol/envelope.py`; the two must agree on every constant.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

/// The only protocol version this build speaks. A child that can't speak it is refused at the
/// handshake rather than half-understood later.
pub const PROTOCOL_VERSION: u32 = 1;
/// The largest single message either side may send, counted as the JSON bytes without the
/// trailing newline. A bigger line ends the child: past it the stream can't be trusted.
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
/// Request IDs are labels, not payloads.
pub const MAX_ID_CHARS: usize = 128;

/// Every message type in protocol v1, in the order a run uses them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    /// App → agent, first message: versions and limits on offer.
    Hello,
    /// Agent → app: the version chosen and what the agent is.
    HelloAck,
    /// App → agent: one task to work on. The envelope `id` is the run ID.
    RunStart,
    /// Agent → app: the run was accepted, under the run's `id`. (The stand-alone tool server uses
    /// the same name for its reply to a `run_start`.)
    RunStartAck,
    /// Agent → app: run one read tool through the Rust tool broker.
    ToolCall,
    /// App → agent: the answer to a `tool_call`, under the same `id`.
    ToolResult,
    /// Agent → app: one model request through the Rust model broker.
    ModelCall,
    /// App → agent: the answer to a `model_call`, under the same `id`.
    ModelResult,
    /// Agent → app: one step of the run's trace.
    TraceEvent,
    /// Agent → app: the run's outcome, under the run's `id`. Ends the run.
    RunResult,
    /// App → agent: stop the named run as soon as possible.
    Cancel,
    /// Either way: something went wrong that isn't an answer to a call.
    Error,
}

impl MessageType {
    /// Whether the agent is allowed to send this type. The app refuses anything else from it.
    pub fn sent_by_agent(self) -> bool {
        matches!(
            self,
            Self::HelloAck
                | Self::RunStartAck
                | Self::ToolCall
                | Self::ModelCall
                | Self::TraceEvent
                | Self::RunResult
                | Self::Error
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub v: u32,
    pub id: String,
    #[serde(rename = "type")]
    pub kind: MessageType,
    pub payload: Value,
}

impl Envelope {
    pub fn new(id: impl Into<String>, kind: MessageType, payload: Value) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id: id.into(),
            kind,
            payload,
        }
    }

    /// The payload field as a string, if it is one.
    pub fn str_field(&self, name: &str) -> Option<&str> {
        self.payload.get(name).and_then(Value::as_str)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    #[error("message of {0} bytes is over the {MAX_MESSAGE_BYTES}-byte limit")]
    TooLarge(usize),
    #[error("message is not a valid envelope: {0}")]
    Malformed(String),
    #[error("message uses protocol version {0}, expected {PROTOCOL_VERSION}")]
    UnsupportedVersion(u32),
    #[error("message id must be 1-{MAX_ID_CHARS} printable ASCII characters")]
    InvalidId,
    #[error("message payload must be a JSON object")]
    PayloadNotObject,
    #[error("the agent may not send `{0:?}` messages")]
    NotAllowed(MessageType),
    #[error("{0}")]
    Unexpected(String),
}

/// One encoded line, newline included. Refuses anything over the size limit so an oversized tool
/// result becomes an error the agent can see instead of a dead link.
pub fn encode(envelope: &Envelope) -> Result<Vec<u8>, ProtocolError> {
    validate_shape(envelope)?;
    let mut bytes = serde_json::to_vec(envelope)
        .map_err(|error| ProtocolError::Malformed(error.to_string()))?;
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(ProtocolError::TooLarge(bytes.len()));
    }
    // serde_json escapes newlines inside strings, so this is the only raw newline in the line.
    bytes.push(b'\n');
    Ok(bytes)
}

/// Decodes one line from the agent and checks everything about it that doesn't depend on what
/// the app was waiting for.
pub fn decode_from_agent(line: &[u8]) -> Result<Envelope, ProtocolError> {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    if line.len() > MAX_MESSAGE_BYTES {
        return Err(ProtocolError::TooLarge(line.len()));
    }
    let envelope: Envelope = serde_json::from_slice(line)
        .map_err(|error| ProtocolError::Malformed(error.to_string()))?;
    validate_shape(&envelope)?;
    if !envelope.kind.sent_by_agent() {
        return Err(ProtocolError::NotAllowed(envelope.kind));
    }
    Ok(envelope)
}

fn validate_shape(envelope: &Envelope) -> Result<(), ProtocolError> {
    if envelope.v != PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion(envelope.v));
    }
    if envelope.id.is_empty()
        || envelope.id.len() > MAX_ID_CHARS
        || !envelope.id.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(ProtocolError::InvalidId);
    }
    if !envelope.payload.is_object() {
        return Err(ProtocolError::PayloadNotObject);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineRead {
    /// A whole line is in the buffer, newline removed.
    Line,
    /// The stream ended. Any partial line left in the buffer was never finished.
    Eof,
    /// The line passed `max` bytes before its newline. The stream is left mid-line.
    TooLong,
}

/// Reads one newline-terminated line without ever holding more than `max` bytes of it, so a child
/// that never sends a newline can't make the app buffer without end.
pub async fn read_bounded_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    max: usize,
    buffer: &mut Vec<u8>,
) -> io::Result<LineRead> {
    buffer.clear();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(LineRead::Eof);
        }
        match available.iter().position(|byte| *byte == b'\n') {
            Some(newline) => {
                if buffer.len() + newline > max {
                    return Ok(LineRead::TooLong);
                }
                buffer.extend_from_slice(&available[..newline]);
                reader.consume(newline + 1);
                return Ok(LineRead::Line);
            }
            None => {
                let taken = available.len();
                if buffer.len() + taken > max {
                    return Ok(LineRead::TooLong);
                }
                buffer.extend_from_slice(available);
                reader.consume(taken);
            }
        }
    }
}

/// Throws away the rest of the current line, newline included, without holding any of it. Used
/// after `LineRead::TooLong`, which leaves the stream mid-line.
pub async fn skip_line<R: AsyncBufRead + Unpin>(reader: &mut R) -> io::Result<()> {
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(());
        }
        match available.iter().position(|byte| *byte == b'\n') {
            Some(newline) => {
                reader.consume(newline + 1);
                return Ok(());
            }
            None => {
                let taken = available.len();
                reader.consume(taken);
            }
        }
    }
}

/// The error codes a `tool_result`, `model_result`, or `error` message can carry. The first eight
/// are the shared tool taxonomy from the overhaul plan and `contracts/tools.v1.json`; the last three belong to
/// the link itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidArguments,
    NotFound,
    Ambiguous,
    PermissionDenied,
    Timeout,
    Unavailable,
    BudgetExhausted,
    Internal,
    /// A message broke the protocol.
    ProtocolError,
    /// The run was cancelled while this call was waiting.
    Cancelled,
    /// The agent is already working on a run.
    Busy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct LinkError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
}

impl LinkError {
    pub fn new(code: ErrorCode, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
        }
    }
}

/// The payload of a `tool_result` or `model_result`.
pub fn result_payload(result: &Result<Value, LinkError>) -> Value {
    match result {
        Ok(value) => serde_json::json!({ "ok": true, "result": value }),
        Err(error) => serde_json::json!({ "ok": false, "error": error }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trips_an_envelope_as_one_line() {
        let envelope = Envelope::new(
            "req-1",
            MessageType::ToolCall,
            json!({ "name": "search_tasks", "text": "two\nlines" }),
        );
        let bytes = encode(&envelope).unwrap();
        assert_eq!(bytes.iter().filter(|byte| **byte == b'\n').count(), 1);
        assert_eq!(decode_from_agent(&bytes).unwrap(), envelope);
    }

    #[test]
    fn uses_the_documented_field_names() {
        let bytes = encode(&Envelope::new("a", MessageType::HelloAck, json!({}))).unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            value,
            json!({ "v": 1, "id": "a", "type": "hello_ack", "payload": {} })
        );
    }

    #[test]
    fn refuses_bad_envelopes() {
        type Check = fn(&ProtocolError) -> bool;
        let cases: [(&str, Check); 7] = [
            (
                r#"{"v":2,"id":"a","type":"run_result","payload":{}}"#,
                |e| matches!(e, ProtocolError::UnsupportedVersion(2)),
            ),
            (r#"{"v":1,"id":"","type":"run_result","payload":{}}"#, |e| {
                matches!(e, ProtocolError::InvalidId)
            }),
            (
                r#"{"v":1,"id":"a b","type":"run_result","payload":{}}"#,
                |e| matches!(e, ProtocolError::InvalidId),
            ),
            (
                r#"{"v":1,"id":"a","type":"run_result","payload":[]}"#,
                |e| matches!(e, ProtocolError::PayloadNotObject),
            ),
            (
                r#"{"v":1,"id":"a","type":"launch_missiles","payload":{}}"#,
                |e| matches!(e, ProtocolError::Malformed(_)),
            ),
            (r#"{"v":1,"id":"a","type":"run_start","payload":{}}"#, |e| {
                matches!(e, ProtocolError::NotAllowed(MessageType::RunStart))
            }),
            (
                r#"{"v":1,"id":"a","type":"error","payload":{},"extra":1}"#,
                |e| matches!(e, ProtocolError::Malformed(_)),
            ),
        ];
        for (line, expected) in cases {
            let error = decode_from_agent(line.as_bytes()).unwrap_err();
            assert!(expected(&error), "{line} gave {error:?}");
        }
    }

    #[test]
    fn refuses_to_encode_an_oversized_message() {
        let big = "x".repeat(MAX_MESSAGE_BYTES);
        let error = encode(&Envelope::new(
            "a",
            MessageType::ToolResult,
            json!({ "ok": true, "result": big }),
        ))
        .unwrap_err();
        assert!(matches!(error, ProtocolError::TooLarge(_)));
    }

    #[tokio::test]
    async fn bounded_reader_stops_at_the_limit() {
        let input = b"short\nthis line is too long\nok\n".to_vec();
        let mut reader = tokio::io::BufReader::with_capacity(4, &input[..]);
        let mut buffer = Vec::new();
        assert_eq!(
            read_bounded_line(&mut reader, 10, &mut buffer)
                .await
                .unwrap(),
            LineRead::Line
        );
        assert_eq!(buffer, b"short");
        assert_eq!(
            read_bounded_line(&mut reader, 10, &mut buffer)
                .await
                .unwrap(),
            LineRead::TooLong
        );
    }

    #[tokio::test]
    async fn an_overlong_line_can_be_skipped() {
        let mut input = vec![b'x'; 50];
        input.extend_from_slice(b"\nnext\n");
        let mut reader = tokio::io::BufReader::with_capacity(16, &input[..]);
        let mut buffer = Vec::new();
        assert_eq!(
            read_bounded_line(&mut reader, 10, &mut buffer)
                .await
                .unwrap(),
            LineRead::TooLong
        );
        skip_line(&mut reader).await.unwrap();
        assert_eq!(
            read_bounded_line(&mut reader, 10, &mut buffer)
                .await
                .unwrap(),
            LineRead::Line
        );
        assert_eq!(buffer, b"next");
    }

    #[tokio::test]
    async fn bounded_reader_reports_the_end() {
        let input = b"done\npartial".to_vec();
        let mut reader = &input[..];
        let mut buffer = Vec::new();
        assert_eq!(
            read_bounded_line(&mut reader, 100, &mut buffer)
                .await
                .unwrap(),
            LineRead::Line
        );
        assert_eq!(
            read_bounded_line(&mut reader, 100, &mut buffer)
                .await
                .unwrap(),
            LineRead::Eof
        );
    }

    #[test]
    fn error_codes_match_the_shared_taxonomy() {
        let error = LinkError::new(ErrorCode::BudgetExhausted, "no more tool calls", false);
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({ "code": "budget_exhausted", "message": "no more tool calls", "retryable": false })
        );
    }
}
