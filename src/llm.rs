use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// System prompt used for every request. The terminal context is appended
/// after it by [`build_messages`].
pub const SYSTEM_PROMPT: &str = "Eres un asistente integrado dentro de un terminal. \
No tienes acceso a herramientas ni puedes ejecutar comandos: solo respondes con texto. \
Cuando propongas comandos, escríbelos en bloques de código vallados (```). \
Sé conciso y directo; evita explicaciones largas salvo que se pidan. \
Responde en el idioma del usuario.";

const HISTORY_LIMIT: usize = 20;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: text.into(),
        }
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: text.into(),
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: text.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamUpdate {
    Delta(String),
    Done,
    Error(String),
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
    ) -> Result<(), String> {
        let url = self.endpoint("chat/completions");
        let payload = serde_json::json!({
            "model": model,
            "stream": true,
            "messages": messages,
        });

        let response = ureq::post(&url)
            .header("Authorization", &self.auth_header())
            .send_json(payload)
            .map_err(|err| format!("POST {url} failed: {err}"))?;

        let mut reader = response.into_body().into_reader();
        let mut chunk = [0u8; 4096];
        let mut pending = String::new();
        let mut utf8_remainder: Vec<u8> = Vec::new();

        loop {
            if self.cancel.load(Ordering::Relaxed) {
                return Ok(());
            }

            let read = match reader.read(&mut chunk) {
                Ok(read) => read,
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
                    let _ = tx.send(StreamUpdate::Done);
                    return Ok(());
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

        let _ = tx.send(StreamUpdate::Done);
        Ok(())
    }
}

fn fail(tx: &Sender<StreamUpdate>, message: String) -> Result<(), String> {
    let _ = tx.send(StreamUpdate::Error(message.clone()));
    Err(message)
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

    let mut recent: Vec<&ChatMessage> = history
        .iter()
        .filter(|message| message.role != Role::System)
        .collect();
    let start = recent.len().saturating_sub(HISTORY_LIMIT);
    recent.drain(..start);
    messages.extend(recent.into_iter().cloned());

    messages
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
