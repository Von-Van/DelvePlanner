//! The only way the Python agent reaches a model: its `model_call` messages come here, and this
//! forwards them to the bundled Ollama on the loopback address with the user's chosen model.
//! Every answer says exactly which model, digest, and runtime version produced it.

use super::protocol::{ErrorCode, LinkError, MAX_MESSAGE_BYTES};
use crate::runtime::ModelIdentity;
use serde_json::{json, Map, Value};
use std::time::{Duration, Instant};

/// The planner's own value: long enough to be warm for a follow-up, short enough to free memory.
const KEEP_ALIVE: &str = "90s";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(190);
const MAX_MESSAGES: usize = 128;
const MAX_CONTEXT_TOKENS: u64 = 32_768;
const MAX_REPLY_TOKENS: u64 = 4_096;
/// Leaves room in the 1 MiB message for the envelope around the reply.
const MAX_REPLY_BYTES: usize = MAX_MESSAGE_BYTES - 16 * 1024;
const MESSAGE_FIELDS: [&str; 5] = ["role", "content", "tool_calls", "tool_name", "thinking"];
const ROLES: [&str; 4] = ["system", "user", "assistant", "tool"];
const NUMBER_OPTIONS: [&str; 8] = [
    "temperature",
    "top_p",
    "top_k",
    "min_p",
    "seed",
    "repeat_penalty",
    "num_ctx",
    "num_predict",
];

#[derive(Clone)]
pub struct ModelBroker {
    client: reqwest::Client,
    endpoint: String,
    identity: ModelIdentity,
}

impl ModelBroker {
    /// Refuses any endpoint that isn't plain HTTP on a loopback IP address: inference stays on
    /// this machine.
    pub fn new(endpoint: &str, identity: ModelIdentity) -> Result<Self, LinkError> {
        let url = reqwest::Url::parse(endpoint).map_err(|_| refuse_endpoint())?;
        // Only an IP address counts: a name like `localhost` goes through a resolver.
        let loopback = url
            .host_str()
            .map(|host| host.trim_start_matches('[').trim_end_matches(']'))
            .and_then(|host| host.parse::<std::net::IpAddr>().ok())
            .is_some_and(|ip| ip.is_loopback());
        if url.scheme() != "http" || !loopback || !url.username().is_empty() {
            return Err(refuse_endpoint());
        }
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .no_proxy()
            .build()
            .map_err(|error| LinkError::new(ErrorCode::Internal, error.to_string(), false))?;
        Ok(Self {
            client,
            endpoint: endpoint.trim_end_matches('/').to_string(),
            identity,
        })
    }

    pub fn identity(&self) -> &ModelIdentity {
        &self.identity
    }

    /// Answers one `model_call` payload with a `model_result` result.
    pub async fn call(&self, request: &Value) -> Result<Value, LinkError> {
        let (body, options) = self.ollama_request(request)?;
        let started = Instant::now();
        let response = self
            .client
            .post(format!("{}/api/chat", self.endpoint))
            .json(&body)
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    LinkError::new(ErrorCode::Timeout, "the local model took too long", true)
                } else {
                    LinkError::new(
                        ErrorCode::Unavailable,
                        "the local AI runtime is not answering",
                        true,
                    )
                }
            })?;
        let status = response.status();
        let bytes = response.bytes().await.map_err(|_| {
            LinkError::new(
                ErrorCode::Unavailable,
                "the local AI runtime stopped mid-reply",
                true,
            )
        })?;
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        if !status.is_success() {
            let detail = serde_json::from_slice::<Value>(&bytes)
                .ok()
                .and_then(|value| value.get("error")?.as_str().map(str::to_string))
                .unwrap_or_else(|| status.to_string());
            let code = if status.is_client_error() {
                ErrorCode::InvalidArguments
            } else {
                ErrorCode::Unavailable
            };
            return Err(LinkError::new(
                code,
                format!("the local model refused the request: {detail}"),
                status.is_server_error(),
            ));
        }
        if bytes.len() > MAX_REPLY_BYTES {
            return Err(LinkError::new(
                ErrorCode::Internal,
                "the model's reply is too large to pass on",
                false,
            ));
        }
        let reply: Value = serde_json::from_slice(&bytes).map_err(|_| {
            LinkError::new(
                ErrorCode::Internal,
                "the local AI runtime sent an unreadable reply",
                false,
            )
        })?;
        let nanos_to_ms = |field: &str| {
            reply
                .get(field)
                .and_then(Value::as_u64)
                .map(|n| n / 1_000_000)
        };
        Ok(json!({
            "model": self.identity.name,
            "digest": self.identity.digest,
            "ollamaVersion": self.identity.ollama_version,
            "message": reply.get("message").cloned().unwrap_or(Value::Null),
            "doneReason": reply.get("done_reason").cloned().unwrap_or(Value::Null),
            "usage": {
                "promptTokens": reply.get("prompt_eval_count").cloned().unwrap_or(Value::Null),
                "completionTokens": reply.get("eval_count").cloned().unwrap_or(Value::Null),
            },
            "timings": {
                "latencyMs": latency_ms,
                "totalMs": nanos_to_ms("total_duration"),
                "loadMs": nanos_to_ms("load_duration"),
            },
            "options": options,
        }))
    }

    /// Builds the Ollama request from the agent's, keeping only what is allowed through.
    fn ollama_request(&self, request: &Value) -> Result<(Value, Value), LinkError> {
        if let Some(model) = request.get("model") {
            if model.as_str() != Some(self.identity.name.as_str()) {
                return Err(invalid(format!(
                    "this run uses `{}`; the agent can't pick another model",
                    self.identity.name
                )));
            }
        }
        let messages = request
            .get("messages")
            .and_then(Value::as_array)
            .filter(|messages| !messages.is_empty() && messages.len() <= MAX_MESSAGES)
            .ok_or_else(|| invalid(format!("`messages` must hold 1-{MAX_MESSAGES} messages")))?;
        let messages = messages
            .iter()
            .map(clean_message)
            .collect::<Result<Vec<_>, _>>()?;

        let mut options = Map::new();
        if let Some(given) = request.get("options") {
            let given = given
                .as_object()
                .ok_or_else(|| invalid("`options` must be an object"))?;
            for (key, value) in given {
                if key == "stop" {
                    let valid = value.as_array().is_some_and(|stops| {
                        stops.len() <= 8 && stops.iter().all(Value::is_string)
                    });
                    if !valid {
                        return Err(invalid("`stop` must be up to 8 strings"));
                    }
                } else if !NUMBER_OPTIONS.contains(&key.as_str()) || !value.is_number() {
                    return Err(invalid(format!("option `{key}` is not allowed")));
                }
                options.insert(key.clone(), value.clone());
            }
        }
        cap(&mut options, "num_ctx", MAX_CONTEXT_TOKENS)?;
        cap(&mut options, "num_predict", MAX_REPLY_TOKENS)?;
        let options = Value::Object(options);

        let mut body = json!({
            "model": self.identity.name,
            "messages": messages,
            "stream": false,
            "keep_alive": KEEP_ALIVE,
            "options": options,
        });
        if let Some(format) = request.get("format") {
            if !(format == "json" || format.is_object()) {
                return Err(invalid("`format` must be \"json\" or a JSON Schema object"));
            }
            body["format"] = format.clone();
        }
        if let Some(tools) = request.get("tools") {
            if !tools.is_array() {
                return Err(invalid("`tools` must be a list"));
            }
            body["tools"] = tools.clone();
        }
        if let Some(think) = request.get("think") {
            if !think.is_boolean() {
                return Err(invalid("`think` must be true or false"));
            }
            body["think"] = think.clone();
        }
        Ok((body, options))
    }
}

fn clean_message(message: &Value) -> Result<Value, LinkError> {
    let message = message
        .as_object()
        .ok_or_else(|| invalid("each message must be an object"))?;
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !ROLES.contains(&role) {
        return Err(invalid(format!("message role `{role}` is not allowed")));
    }
    if !message.get("content").is_some_and(Value::is_string) {
        return Err(invalid("each message needs text `content`"));
    }
    if let Some(key) = message
        .keys()
        .find(|key| !MESSAGE_FIELDS.contains(&key.as_str()))
    {
        return Err(invalid(format!("message field `{key}` is not allowed")));
    }
    Ok(Value::Object(message.clone()))
}

fn cap(options: &mut Map<String, Value>, key: &str, max: u64) -> Result<(), LinkError> {
    if let Some(value) = options.get(key) {
        match value.as_u64() {
            Some(n) if (1..=max).contains(&n) => {}
            _ => {
                return Err(invalid(format!(
                    "`{key}` must be a whole number from 1 to {max}"
                )))
            }
        }
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> LinkError {
    LinkError::new(ErrorCode::InvalidArguments, message, false)
}

fn refuse_endpoint() -> LinkError {
    LinkError::new(
        ErrorCode::PermissionDenied,
        "the model broker only talks to the local AI runtime on this machine",
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn identity() -> ModelIdentity {
        ModelIdentity {
            name: "qwen3:8b".into(),
            digest: "a".repeat(64),
            ollama_version: "0.32.0".into(),
        }
    }

    /// A one-request stand-in for Ollama: hands back what it was sent and replies with `reply`.
    async fn fake_ollama(
        status: &'static str,
        reply: Value,
    ) -> (String, tokio::task::JoinHandle<Value>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 4096];
            let body_start = loop {
                let read = socket.read(&mut chunk).await.unwrap();
                request.extend_from_slice(&chunk[..read]);
                if let Some(at) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    break at + 4;
                }
            };
            let head = String::from_utf8_lossy(&request[..body_start]).to_lowercase();
            let length: usize = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .map(|value| value.trim().parse().unwrap())
                .unwrap_or(0);
            while request.len() < body_start + length {
                let read = socket.read(&mut chunk).await.unwrap();
                request.extend_from_slice(&chunk[..read]);
            }
            let body = reply.to_string();
            let response = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            serde_json::from_slice(&request[body_start..]).unwrap()
        });
        (endpoint, handle)
    }

    #[test]
    fn only_talks_to_loopback() {
        for endpoint in [
            "http://example.com:11434",
            "http://10.0.0.5:11434",
            "https://127.0.0.1:11434",
            "http://localhost:11434",
            "http://user:pw@127.0.0.1:1",
            "not a url",
        ] {
            let error = ModelBroker::new(endpoint, identity()).err().unwrap();
            assert_eq!(error.code, ErrorCode::PermissionDenied, "{endpoint}");
        }
        assert!(ModelBroker::new("http://127.0.0.1:49152", identity()).is_ok());
        assert!(ModelBroker::new("http://[::1]:49152", identity()).is_ok());
    }

    #[tokio::test]
    async fn forwards_a_clean_request_and_labels_the_reply() {
        let (endpoint, sent) = fake_ollama(
            "200 OK",
            json!({
                "model": "qwen3:8b",
                "message": { "role": "assistant", "content": "{\"ok\":true}" },
                "done_reason": "stop",
                "prompt_eval_count": 12,
                "eval_count": 5,
                "total_duration": 2_500_000_000u64,
                "load_duration": 1_000_000_000u64,
            }),
        )
        .await;
        let broker = ModelBroker::new(&endpoint, identity()).unwrap();
        let result = broker
            .call(&json!({
                "runId": "run-1",
                "messages": [{ "role": "user", "content": "hi" }],
                "format": "json",
                "options": { "temperature": 0, "seed": 7, "num_ctx": 8192 },
            }))
            .await
            .unwrap();

        let sent = sent.await.unwrap();
        assert_eq!(sent["model"], "qwen3:8b");
        assert_eq!(sent["stream"], false);
        assert_eq!(sent["format"], "json");
        assert_eq!(sent["options"]["seed"], 7);
        // The run ID is the link's business, not Ollama's.
        assert!(sent.get("runId").is_none());

        assert_eq!(result["model"], "qwen3:8b");
        assert_eq!(result["digest"], "a".repeat(64));
        assert_eq!(result["ollamaVersion"], "0.32.0");
        assert_eq!(result["message"]["content"], "{\"ok\":true}");
        assert_eq!(result["usage"]["completionTokens"], 5);
        assert_eq!(result["timings"]["loadMs"], 1000);
        assert_eq!(result["options"]["temperature"], 0);
    }

    #[tokio::test]
    async fn refuses_requests_outside_the_allowlist() {
        let broker = ModelBroker::new("http://127.0.0.1:1", identity()).unwrap();
        let user = json!([{ "role": "user", "content": "hi" }]);
        for request in [
            json!({ "messages": [] }),
            json!({ "messages": user, "model": "llama3.1:70b" }),
            json!({ "messages": [{ "role": "root", "content": "x" }] }),
            json!({ "messages": [{ "role": "user", "content": "x", "images": ["…"] }] }),
            json!({ "messages": user, "options": { "num_gpu": 99 } }),
            json!({ "messages": user, "options": { "num_ctx": 1_000_000 } }),
            json!({ "messages": user, "format": "yaml" }),
        ] {
            let error = broker.call(&request).await.unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidArguments, "{request}");
        }
    }

    #[tokio::test]
    async fn reports_an_unreachable_runtime_as_retryable() {
        // Nothing listens on port 1.
        let broker = ModelBroker::new("http://127.0.0.1:1", identity()).unwrap();
        let error = broker
            .call(&json!({ "messages": [{ "role": "user", "content": "hi" }] }))
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Unavailable);
        assert!(error.retryable);
    }

    #[tokio::test]
    async fn passes_on_ollamas_own_refusal() {
        let (endpoint, _sent) =
            fake_ollama("404 Not Found", json!({ "error": "model not found" })).await;
        let broker = ModelBroker::new(&endpoint, identity()).unwrap();
        let error = broker
            .call(&json!({ "messages": [{ "role": "user", "content": "hi" }] }))
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArguments);
        assert!(error.message.contains("model not found"));
    }
}
