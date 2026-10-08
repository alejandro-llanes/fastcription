//! Asking the meaning server what a word or expression means.
//!
//! The server is Ollama, running a small instruction-tuned model on a machine
//! with a GPU — the same box, and the same arrangement, as the transcription
//! server. One request carries the expression and the transcript line it was
//! said in, and the answer is three things: a short English meaning *in that
//! context*, the expression in the reader's own language, and an example.
//!
//! Why a language model and not a dictionary: a dictionary has "table" and
//! has "ocean", and has nothing for "table this" or "boil the ocean", which
//! are exactly the things a non-native listener gets lost on. Measured before
//! this was built (docs/SERVER.md): `gemma3:4b` answers in about 0.4 s on an
//! RTX 5070 beside the whisper model, and was the only model of the three
//! tried whose Spanish could be trusted.
//!
//! Ollama's native `/api/chat` rather than its OpenAI-compatible endpoint,
//! for two things the compatible one lacks: `format: "json"`, which makes the
//! model emit valid JSON rather than JSON wrapped in prose or code fences, and
//! `keep_alive`, which stops it unloading the model after a few idle minutes
//! and charging the next lookup five seconds to load it again.

use std::time::Duration;

/// Where the server is and what to ask it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// `http://host:port`, no path.
    pub endpoint: String,
    /// An Ollama model tag, e.g. `gemma3:4b`.
    pub model: String,
    /// The reader's language, in English, e.g. `Spanish`.
    pub language: String,
}

/// What the server said about an expression.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Answer {
    pub meaning: String,
    pub translation: String,
    pub example: String,
}

/// How long the model stays loaded after a request. Long: the app re-warms
/// it well inside this while it is running, and a loaded model costs VRAM
/// the card has to spare rather than time the reader does not.
const KEEP_ALIVE: &str = "1h";

/// Generation is sub-second on a GPU; this is for a server that is up but
/// answering from a CPU, which can take a while, and for a dead network.
const TIMEOUT: Duration = Duration::from_secs(60);

/// The answer is three short fields. Capped so a model that starts
/// explaining cannot run on for a minute.
const MAX_TOKENS: u32 = 200;

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(TIMEOUT).build()
}

fn url(endpoint: &str, path: &str) -> String {
    format!("{}{path}", endpoint.trim().trim_end_matches('/'))
}

/// What the model is for, and the shape it answers in.
///
/// The meaning is asked for in English rather than in the reader's language
/// on purpose: the translation of an idiom from a small model is sometimes
/// literal, and an English meaning beside it is what lets the reader catch
/// that. It is also the thing they are trying to learn.
pub fn system_prompt(language: &str) -> String {
    format!(
        "You help a {language} speaker understand spoken English. Given an expression, \
         and usually the transcript line it was said in, reply with a JSON object with \
         exactly these keys: \"meaning\" (a short English meaning of the expression as used in this \
         line), \"translation\" (a natural {language} translation of the expression, not \
         word by word), \"example\" (one short English example sentence using it). \
         Be concise. Output JSON only."
    )
}

pub fn user_prompt(expression: &str, context: &str) -> String {
    // An imported word has no line. Sending an empty one would invite the
    // model to explain the word in the light of nothing in particular.
    if context.trim().is_empty() {
        format!("Expression: \"{expression}\"")
    } else {
        format!("Line: \"{context}\"\nExpression: \"{expression}\"")
    }
}

/// The request body for one lookup.
pub fn request_body(config: &Config, expression: &str, context: &str) -> String {
    serde_json::json!({
        "model": config.model,
        "stream": false,
        "format": "json",
        "keep_alive": KEEP_ALIVE,
        "options": { "temperature": 0, "num_predict": MAX_TOKENS },
        "messages": [
            { "role": "system", "content": system_prompt(&config.language) },
            { "role": "user", "content": user_prompt(expression, context) }
        ]
    })
    .to_string()
}

/// Asks the server. Blocking; run it on a thread.
pub fn look_up(config: &Config, expression: &str, context: &str) -> Result<Answer, String> {
    let body = request_body(config, expression, context);
    let text = post(&url(&config.endpoint, "/api/chat"), &body)?;
    let reply: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("The server's reply was not JSON: {e}"))?;
    let content = reply
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .ok_or_else(|| "The server's reply had no message in it".to_owned())?;
    parse_answer(content)
}

/// Loads the model without asking it anything, so the first real lookup does
/// not pay to load it. Ollama documents an empty `messages` array as exactly
/// this.
pub fn warm(config: &Config) -> Result<(), String> {
    let body = serde_json::json!({
        "model": config.model,
        "messages": [],
        "keep_alive": KEEP_ALIVE,
    })
    .to_string();
    post(&url(&config.endpoint, "/api/chat"), &body).map(|_| ())
}

/// Whether the server is there and has the model: what the settings pane's
/// test button reports.
pub fn check(config: &Config) -> Result<String, String> {
    let text = agent()
        .get(&url(&config.endpoint, "/api/tags"))
        .call()
        .map_err(describe)?
        .into_string()
        .map_err(|e| format!("Could not read the server's reply: {e}"))?;
    let tags: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("The server's reply was not JSON: {e}"))?;
    let names: Vec<String> = tags
        .get("models")
        .and_then(|m| m.as_array())
        .map(|models| {
            models
                .iter()
                .filter_map(|m| m.get("name").and_then(|n| n.as_str()))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let wanted = config.model.trim();
    if names.iter().any(|n| n == wanted) {
        Ok(format!("Reached the server; {wanted} is pulled"))
    } else {
        Err(format!(
            "Reached the server, but {wanted} is not pulled. On that machine: ollama pull {wanted}"
        ))
    }
}

fn post(url: &str, body: &str) -> Result<String, String> {
    agent()
        .post(url)
        .set("Content-Type", "application/json")
        .send_string(body)
        .map_err(describe)?
        .into_string()
        .map_err(|e| format!("Could not read the server's reply: {e}"))
}

/// An error a reader can act on: a refused connection names the address, an
/// HTTP error carries Ollama's own message, which names the model.
fn describe(err: ureq::Error) -> String {
    match err {
        ureq::Error::Status(code, response) => {
            let body = response.into_string().unwrap_or_default();
            let detail = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_owned))
                .unwrap_or(body);
            format!("The server answered {code}: {}", detail.trim())
        }
        ureq::Error::Transport(transport) => format!("Could not reach the server: {transport}"),
    }
}

/// The model's content, as an [`Answer`].
///
/// `format: "json"` makes this reliable, but the parser is still lenient —
/// code fences, text around the object, a missing key — because a different
/// server than the one this was tested against costs nothing to tolerate and
/// an answer thrown away over a fence is a lookup the reader waited for.
pub fn parse_answer(content: &str) -> Result<Answer, String> {
    let start = content.find('{');
    let end = content.rfind('}');
    let (Some(start), Some(end)) = (start, end) else {
        return Err("The model did not answer with JSON".to_owned());
    };
    if end < start {
        return Err("The model did not answer with JSON".to_owned());
    }
    let value: serde_json::Value = serde_json::from_str(&content[start..=end])
        .map_err(|e| format!("The model's answer was not valid JSON: {e}"))?;
    let field = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_owned())
            .unwrap_or_default()
    };
    let answer = Answer {
        meaning: field("meaning"),
        translation: field("translation"),
        example: field("example"),
    };
    if answer.meaning.is_empty() && answer.translation.is_empty() {
        return Err("The model answered without a meaning or a translation".to_owned());
    }
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn config(endpoint: &str) -> Config {
        Config {
            endpoint: endpoint.to_owned(),
            model: "gemma3:4b".to_owned(),
            language: "Spanish".to_owned(),
        }
    }

    #[test]
    fn a_clean_answer_parses() {
        let answer = parse_answer(
            r#"{"meaning": "a rough estimate", "translation": "una cifra aproximada", "example": "Give me a ballpark figure."}"#,
        )
        .unwrap();
        assert_eq!(answer.meaning, "a rough estimate");
        assert_eq!(answer.translation, "una cifra aproximada");
        assert_eq!(answer.example, "Give me a ballpark figure.");
    }

    /// What `qwen2.5:1.5b` did in the benchmark: the object inside a code
    /// fence. Thrown away, that is a lookup the reader waited for.
    #[test]
    fn a_fenced_answer_parses() {
        let answer = parse_answer(
            "```json\n{\"meaning\": \"postpone\", \"translation\": \"posponer\"}\n```",
        )
        .unwrap();
        assert_eq!(answer.meaning, "postpone");
        assert_eq!(answer.example, "");
    }

    #[test]
    fn prose_and_a_missing_key_are_tolerated_but_nothing_useful_is_not() {
        let answer =
            parse_answer("Sure! {\"translation\": \"hervir el mar\"} Hope it helps.").unwrap();
        assert_eq!(answer.translation, "hervir el mar");
        assert!(parse_answer("I don't know.").is_err());
        assert!(parse_answer("{}").is_err());
        assert!(parse_answer("{\"example\": \"only this\"}").is_err());
        assert!(parse_answer("} {").is_err());
    }

    /// The prompt is the feature: both inputs and the language have to reach
    /// the model, and the answer has to be asked for as JSON.
    #[test]
    fn the_request_carries_everything_the_model_needs() {
        let body = request_body(
            &config("http://x"),
            "table this",
            "Let's table this for now.",
        );
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["model"], "gemma3:4b");
        assert_eq!(v["format"], "json");
        assert_eq!(v["stream"], false);
        assert_eq!(v["keep_alive"], KEEP_ALIVE);
        let text = body.to_lowercase();
        assert!(text.contains("spanish"));
        assert!(text.contains("table this"));
        assert!(text.contains("let's table this for now."));
    }

    #[test]
    fn an_imported_word_is_asked_about_without_a_line() {
        assert!(!user_prompt("sandbag", "").contains("Line:"));
        assert!(user_prompt("sandbag", "Let's not sandbag the estimate.").contains("Line:"));
    }

    #[test]
    fn the_endpoint_may_carry_a_trailing_slash() {
        assert_eq!(url("http://h:1/", "/api/chat"), "http://h:1/api/chat");
        assert_eq!(url(" http://h:1 ", "/api/tags"), "http://h:1/api/tags");
    }

    /// Serves one request and returns it, so the test can see what the
    /// client sent as well as what it made of the reply.
    fn serve_once(
        status: &'static str,
        reply: &'static str,
    ) -> (u16, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let handle = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("accept");
            let mut raw = Vec::new();
            let mut buf = [0u8; 8192];
            let mut header_end = None;
            while header_end.is_none() {
                let n = sock.read(&mut buf).expect("read");
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&buf[..n]);
                header_end = raw.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4);
            }
            let head = String::from_utf8_lossy(&raw[..header_end.unwrap_or(raw.len())]).to_string();
            let length = head
                .lines()
                .find_map(|l| {
                    l.strip_prefix("Content-Length: ")
                        .or_else(|| l.strip_prefix("content-length: "))
                })
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            let have = raw.len() - header_end.unwrap_or(raw.len());
            let mut body = raw[header_end.unwrap_or(raw.len())..].to_vec();
            if have < length {
                let mut rest = vec![0u8; length - have];
                sock.read_exact(&mut rest).expect("body");
                body.extend_from_slice(&rest);
            }
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            sock.write_all(response.as_bytes()).expect("write");
            format!("{head}{}", String::from_utf8_lossy(&body))
        });
        (port, handle)
    }

    #[test]
    fn a_lookup_speaks_ollamas_chat_api_and_reads_the_answer() {
        let (port, server) = serve_once(
            "200 OK",
            r#"{"model":"gemma3:4b","message":{"role":"assistant","content":"{\"meaning\": \"to try to do too much at once\", \"translation\": \"intentar hacer demasiado a la vez\", \"example\": \"Don't boil the ocean.\"}"},"done":true}"#,
        );
        let answer = look_up(
            &config(&format!("http://127.0.0.1:{port}")),
            "boil the ocean",
            "I don't want to boil the ocean here.",
        )
        .expect("lookup");
        assert_eq!(answer.translation, "intentar hacer demasiado a la vez");
        let request = server.join().unwrap();
        assert!(request.starts_with("POST /api/chat HTTP/1.1"), "{request}");
        assert!(request.contains("\"format\":\"json\""), "{request}");
        assert!(request.contains("boil the ocean"), "{request}");
    }

    /// Ollama's own error, not a bare status code: the message names the
    /// model, which is the thing the reader has to go and pull.
    #[test]
    fn a_missing_model_is_reported_in_the_servers_words() {
        let (port, _server) = serve_once(
            "404 Not Found",
            r#"{"error":"model 'gemma3:4b' not found"}"#,
        );
        let err = look_up(&config(&format!("http://127.0.0.1:{port}")), "x", "y").unwrap_err();
        assert!(err.contains("404") && err.contains("gemma3:4b"), "{err}");
    }

    #[test]
    fn a_dead_server_is_a_readable_error() {
        let err = look_up(&config("http://127.0.0.1:1"), "x", "y").unwrap_err();
        assert!(err.starts_with("Could not reach the server"), "{err}");
    }

    #[test]
    fn the_check_says_whether_the_model_is_pulled() {
        let (port, _s) = serve_once(
            "200 OK",
            r#"{"models":[{"name":"gemma3:4b"},{"name":"qwen2.5:3b"}]}"#,
        );
        assert!(check(&config(&format!("http://127.0.0.1:{port}"))).is_ok());
        let (port, _s) = serve_once("200 OK", r#"{"models":[{"name":"qwen2.5:3b"}]}"#);
        let err = check(&config(&format!("http://127.0.0.1:{port}"))).unwrap_err();
        assert!(err.contains("ollama pull gemma3:4b"), "{err}");
    }

    /// Against the real thing. Run with `--ignored` on a machine with Ollama
    /// and the model; this is the benchmark that chose the model, kept.
    #[test]
    #[ignore = "needs Ollama at 127.0.0.1:11434 with gemma3:4b pulled"]
    fn the_real_server_explains_an_idiom_in_spanish() {
        let answer = look_up(
            &config("http://127.0.0.1:11434"),
            "ballpark figure",
            "That ballpark figure is fine for the pitch deck.",
        )
        .expect("lookup");
        assert!(!answer.meaning.is_empty());
        assert!(!answer.translation.is_empty(), "{answer:?}");
    }
}
