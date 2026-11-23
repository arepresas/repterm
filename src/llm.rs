use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::tools::ToolCall;

/// System prompt used for every request. The terminal context is appended
/// after it by [`build_messages`].
pub const SYSTEM_PROMPT: &str = "Eres un asistente integrado dentro de un terminal. \
Puedes usar las herramientas list_dir, read_file, write_file y edit_file para trabajar \
en archivos. Solo puedes acceder al directorio de la pestaña activa; para cualquier \
ruta fuera de él se pedirá permiso al usuario. No ejecutes comandos de shell por tu cuenta; \
cuando propongas uno, escríbelo en un bloque de código vallado (```). \
Incluye siempre un `reason` breve que explique por qué usas cada herramienta. \
Sé conciso y directo; evita explicaciones largas salvo que se pidan. \
Responde en el idioma del usuario.";

const HISTORY_LIMIT: usize = 20;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "serialize_tool_calls", deserialize_with = "deserialize_tool_calls")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct WireFunction {
    name: String,
    arguments: String,
}

#[derive(Serialize, Deserialize)]
struct WireToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: WireFunction,
}

fn serialize_tool_calls<S>(value: &Option<Vec<ToolCall>>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let wire = value.as_ref().map(|calls| {
        calls
            .iter()
            .map(|call| WireToolCall {
                id: call.id.clone(),
                kind: "function".to_string(),
                function: WireFunction {
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                },
            })
            .collect::<Vec<_>>()
    });
    wire.serialize(serializer)
}

fn deserialize_tool_calls<'de, D>(deserializer: D) -> Result<Option<Vec<ToolCall>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let wire = Option::<Vec<WireToolCall>>::deserialize(deserializer)?;
    Ok(wire.map(|calls| {
        calls
            .into_iter()
            .map(|call| ToolCall {
                id: call.id,
                name: call.function.name,
                arguments: call.function.arguments,
            })
            .collect()
    }))
}

impl ChatMessage {
    fn plain(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            content: text.into(),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn system(text: impl Into<String>) -> Self {
        Self::plain(Role::System, text)
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self::plain(Role::User, text)
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self::plain(Role::Assistant, text)
    }

    pub fn assistant_tool_calls(calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: Some(calls),
            tool_call_id: None,
        }
    }

    pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: None,
            tool_call_id: Some(call_id.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamUpdate {
    Delta(String),
    ToolCalls(Vec<ToolCall>),
    Done,
    Error(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallFragment {
    pub index: usize,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: Option<String>,
}

#[derive(Clone)]
pub struct LlmClient {
    pub base_url: String,
    pub api_key: String,
    pub cancel: Arc<AtomicBool>,
}

impl std::fmt::Debug for LlmClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmClient")
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .field("cancel", &self.cancel)
            .finish()
    }
}

impl LlmClient {
    fn endpoint(&self, suffix: &str) -> String {
        format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            suffix.trim_start_matches('/')
        )
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.api_key)
    }

    /// Lists the model ids served by the proxy (`GET {base_url}/models`).
    pub fn models(&self) -> Result<Vec<String>, String> {
        let url = self.endpoint("models");
        let mut request = ureq::get(&url);
        if !self.api_key.is_empty() {
            request = request.header("Authorization", &self.auth_header());
        }

        let mut response = request
            .call()
            .map_err(|err| format!("GET {url} failed: {err}"))?;
        let body: serde_json::Value = response
            .body_mut()
            .read_json()
            .map_err(|err| format!("GET {url}: invalid JSON: {err}"))?;

        let ids = body
            .get("data")
            .and_then(|data| data.as_array())
            .map(|models| {
                models
                    .iter()
                    .filter_map(|model| model.get("id").and_then(|id| id.as_str()))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        Ok(ids)
    }

    /// Streams a chat completion, sending [`StreamUpdate`]s through `tx`.
    ///
    /// Every returned `Err` is also reported as [`StreamUpdate::Error`] on the
    /// channel, so callers can rely on the channel alone.
    pub fn stream(
        &self,
        model: &str,
        messages: &[ChatMessage],
        tx: &Sender<StreamUpdate>,
        tools: &[serde_json::Value],
    ) -> Result<(), String> {
        let url = self.endpoint("chat/completions");
        let payload = completion_payload(model, messages, tools);

        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_recv_response(Some(std::time::Duration::from_secs(30)))
            .timeout_per_read(Some(std::time::Duration::from_secs(30)))
            .build()
            .into();
        let mut response = agent
            .post(&url)
            .header("Authorization", &self.auth_header())
            .send_json(payload)
            .map_err(|err| format!("POST {url} failed: {err}"))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.body_mut().read_to_string().unwrap_or_default();
            let body = body.trim();
            let detail = if body.is_empty() {
                format!("POST {url} failed: {status}")
            } else {
                let body: String = body.chars().take(500).collect();
                format!("POST {url} failed: {status}: {body}")
            };
            return fail(tx, detail);
        }

        let mut reader = response.into_body().into_reader();
        let mut chunk = [0u8; 4096];
        let mut pending = String::new();
        let mut utf8_remainder: Vec<u8> = Vec::new();
        let mut tool_fragments: Vec<ToolCallFragment> = Vec::new();

        loop {
            if self.cancel.load(Ordering::Relaxed) {
                return Ok(());
            }

            let read = match reader.read(&mut chunk) {
                Ok(read) => read,
                Err(err)
                    if self.cancel.load(Ordering::Relaxed)
                        || err.kind() == std::io::ErrorKind::TimedOut =>
                {
                    continue;
                }
                Err(err) => return fail(tx, format!("read error: {err}")),
            };
            if read == 0 {
                break;
            }

            utf8_remainder.extend_from_slice(&chunk[..read]);
            match std::str::from_utf8(&utf8_remainder) {
                Ok(text) => {
                    pending.push_str(text);
                    utf8_remainder.clear();
                }
                Err(err) => {
                    let valid = err.valid_up_to();
                    if let Ok(text) = std::str::from_utf8(&utf8_remainder[..valid]) {
                        pending.push_str(text);
                    }
                    utf8_remainder.drain(..valid);
                }
            }

            let (frames, rest) = parse_sse_frames(&pending);
            pending = rest;

            for frame in frames {
                if frame.trim() == "[DONE]" {
                    if tool_fragments.is_empty() {
                        let _ = tx.send(StreamUpdate::Done);
                    } else {
                        let _ = tx.send(StreamUpdate::ToolCalls(aggregate_tool_calls(
                            &tool_fragments,
                        )));
                    }
                    return Ok(());
                }
                match parse_tool_call_delta(&frame) {
                    Ok(fragments) => tool_fragments.extend(fragments),
                    Err(err) => return fail(tx, err),
                }
                match parse_delta(&frame) {
                    Ok(Some(delta)) => {
                        let _ = tx.send(StreamUpdate::Delta(delta));
                    }
                    Ok(None) => {}
                    Err(err) => return fail(tx, err),
                }
            }
        }

        if tool_fragments.is_empty() {
            let _ = tx.send(StreamUpdate::Done);
        } else {
            let _ = tx.send(StreamUpdate::ToolCalls(aggregate_tool_calls(
                &tool_fragments,
            )));
        }
        Ok(())
    }
}

fn fail(tx: &Sender<StreamUpdate>, message: String) -> Result<(), String> {
    let _ = tx.send(StreamUpdate::Error(message.clone()));
    Err(message)
}

/// Extracts incremental `tool_calls` from one SSE delta frame.
pub fn parse_tool_call_delta(frame: &str) -> Result<Vec<ToolCallFragment>, String> {
    let value: serde_json::Value = serde_json::from_str(frame.trim())
        .map_err(|err| format!("invalid SSE frame: {err}"))?;
    let Some(fragments) = value.pointer("/choices/0/delta/tool_calls").and_then(|v| v.as_array())
    else {
        return Ok(Vec::new());
    };

    let mut result = Vec::new();
    for fragment in fragments {
        let Some(index) = fragment.get("index").and_then(|v| v.as_u64()) else {
            continue;
        };
        let name = fragment
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let arguments = fragment
            .get("function")
            .and_then(|f| f.get("arguments"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let id = fragment
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        result.push(ToolCallFragment {
            index: index as usize,
            id,
            name,
            arguments,
        });
    }
    Ok(result)
}

/// Groups SSE fragments by index, preserving first-seen order.
pub fn aggregate_tool_calls(fragments: &[ToolCallFragment]) -> Vec<ToolCall> {
    let mut calls: Vec<(usize, ToolCall)> = Vec::new();
    for fragment in fragments {
        let entry = if let Some(entry) = calls.iter_mut().find(|(index, _)| *index == fragment.index)
        {
            entry
        } else {
            calls.push((
                fragment.index,
                ToolCall {
                    id: String::new(),
                    name: String::new(),
                    arguments: String::new(),
                },
            ));
            calls.last_mut().unwrap()
        };
        let call = &mut entry.1;
        if call.id.is_empty() {
            if let Some(id) = &fragment.id {
                call.id = id.clone();
            }
        }
        if let Some(name) = &fragment.name {
            call.name.push_str(name);
        }
        if let Some(arguments) = &fragment.arguments {
            call.arguments.push_str(arguments);
        }
    }
    calls.into_iter().map(|(_, call)| call).collect()
}

/// Builds the OpenAI-compatible streaming request payload.
pub fn completion_payload(
    model: &str,
    messages: &[ChatMessage],
    tools: &[serde_json::Value],
) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "model": model,
        "stream": true,
        "messages": messages,
    });
    if !tools.is_empty() {
        payload["tools"] = serde_json::json!(tools);
        payload["tool_choice"] = serde_json::json!("auto");
    }
    payload
}

/// Splits complete SSE blocks (`\n\n`-separated) into their `data:` payloads,
/// returning the frames and the incomplete remainder.
pub fn parse_sse_frames(input: &str) -> (Vec<String>, String) {
    let mut frames = Vec::new();
    let mut rest = input;

    while let Some(end) = rest.find("\n\n") {
        let block = &rest[..end];
        for line in block.lines() {
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim_start().trim_end_matches('\r');
                frames.push(data.to_string());
            }
        }
        rest = &rest[end + 2..];
    }

    (frames, rest.to_string())
}

/// Parses one SSE frame. `Ok(None)` means the frame carries no content (for
/// example `[DONE]` or a role-only delta); an `Err` carries the API error text.
pub fn parse_delta(frame: &str) -> Result<Option<String>, String> {
    let frame = frame.trim();
    if frame == "[DONE]" {
        return Ok(None);
    }

    let value: serde_json::Value =
        serde_json::from_str(frame).map_err(|err| format!("invalid SSE frame: {err}"))?;

    if let Some(message) = value
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(|message| message.as_str())
    {
        return Err(message.to_string());
    }

    let content = value
        .pointer("/choices/0/delta/content")
        .and_then(|content| content.as_str());

    Ok(content.map(str::to_string))
}

/// Builds the request messages: a fresh system message (prompt + terminal
/// context) followed by the last [`HISTORY_LIMIT`] non-system messages.
pub fn build_messages(context: &str, history: &[ChatMessage]) -> Vec<ChatMessage> {
    let mut messages = Vec::with_capacity(HISTORY_LIMIT + 1);
    messages.push(ChatMessage::system(format!(
        "{SYSTEM_PROMPT}\n\nCONTEXTO:\n{context}"
    )));

    let mut units: Vec<Vec<&ChatMessage>> = Vec::new();
    let mut carrying_tools = false;
    for message in history.iter().filter(|m| m.role != Role::System) {
        if carrying_tools && message.role == Role::Tool {
            units.last_mut().unwrap().push(message);
            continue;
        }
        carrying_tools = message.role == Role::Assistant && message.tool_calls.is_some();
        units.push(vec![message]);
    }

    let start = units.len().saturating_sub(HISTORY_LIMIT);
    for unit in units.into_iter().skip(start) {
        messages.extend(unit.into_iter().cloned());
    }

    messages
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{tool_definitions, ToolCall};

    #[test]
    fn parse_sse_frames_handles_split_and_keeps_remainder() {
        let (frames, rest) = parse_sse_frames("data: {\"a\":1}\n\ndata: {\"b\":2}\n\nda");
        assert_eq!(frames, vec!["{\"a\":1}", "{\"b\":2}"]);
        assert_eq!(rest, "da");
    }

    #[test]
    fn parse_delta_extracts_text_done_and_errors() {
        assert_eq!(
            parse_delta("{\"choices\":[{\"delta\":{\"content\":\"hola\"}}]}").unwrap(),
            Some("hola".into())
        );
        assert_eq!(parse_delta("[DONE]").unwrap(), None);
        assert!(parse_delta("{\"error\":{\"message\":\"boom\"}}").is_err());
    }

    #[test]
    fn debug_output_redacts_api_key() {
        let client = LlmClient {
            base_url: "http://localhost:4000/v1".into(),
            api_key: "sk-secret".into(),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let text = format!("{client:?}");
        assert!(text.contains("<redacted>"));
        assert!(!text.contains("sk-secret"));
    }

    #[test]
    fn build_messages_refreshes_context_and_caps_history() {
        let history: Vec<_> = (0..30)
            .map(|i| ChatMessage::user(format!("m{i}")))
            .collect();
        let msgs = build_messages("CTX", &history);
        assert_eq!(msgs.len(), 21);
        assert!(matches!(msgs[0].role, Role::System));
        assert!(msgs[0].content.contains("CTX"));
        assert_eq!(msgs[1].content, "m10");
    }

    #[test]
    fn aggregates_split_tool_call_fragments() {
        let frames = [
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read_","arguments":""}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"file","arguments":"{\"path\":"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a.txt\"}"}}]}}]}"#,
        ];
        let mut fragments = Vec::new();
        for frame in frames {
            fragments.extend(parse_tool_call_delta(frame).unwrap());
        }
        let calls = aggregate_tool_calls(&fragments);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments, r#"{"path":"a.txt"}"#);
    }

    #[test]
    fn build_messages_keeps_tool_call_pairs() {
        let history = vec![
            ChatMessage::user("lee el archivo"),
            ChatMessage::assistant_tool_calls(vec![ToolCall {
                id: "c1".into(),
                name: "read_file".into(),
                arguments: "{}".into(),
            }]),
            ChatMessage::tool_result("c1", "contenido"),
        ];
        let msgs = build_messages("CTX", &history);
        let roles: Vec<_> = msgs.iter().skip(1).map(|m| m.role).collect();
        assert_eq!(roles, vec![Role::User, Role::Assistant, Role::Tool]);
    }

    #[test]
    fn completion_payload_includes_tools_when_nonempty() {
        let defs = tool_definitions();
        let payload = completion_payload("m", &[ChatMessage::user("hola")], &defs);
        assert_eq!(payload["stream"], true);
        assert_eq!(payload["tool_choice"], "auto");
        assert_eq!(payload["tools"].as_array().unwrap().len(), 4);
        let empty = completion_payload("m", &[], &[]);
        assert!(empty.get("tools").is_none());
    }

    #[test]
    fn system_prompt_describes_available_tools() {
        assert!(SYSTEM_PROMPT.contains("list_dir"));
        assert!(SYSTEM_PROMPT.contains("read_file"));
        assert!(SYSTEM_PROMPT.contains("reason"));
    }
}
