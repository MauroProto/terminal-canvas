//! Servidor de hooks de agentes (P2.12, T1).
//!
//! Escucha en `127.0.0.1:0` (puerto efímero) y expone `POST /hook/:provider`
//! protegido por el header `X-TC-Token`. En cada arranque escribe un *endpoint
//! file* (`<data>/agent-hooks/endpoint.sh`) con `TC_HOOK_URL` y `TC_HOOK_TOKEN`
//! para que los hooks del agente (que corren en otro proceso) sepan a dónde
//! postear sin hardcodear nada.
//!
//! Los eventos llegan al loop de la app por un canal mpsc que se drena en
//! `begin_frame`.

use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::Router;
use serde::Deserialize;
use uuid::Uuid;

/// Header con el token compartido; sin él (o con el token equivocado) el
/// request se rechaza con 401.
pub const TOKEN_HEADER: &str = "X-TC-Token";

/// Tipo de evento de hook, normalizado entre providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookKind {
    /// El agente terminó su turno.
    Stop,
    /// El usuario mandó un prompt (el agente arranca a trabajar).
    UserPromptSubmit,
    /// El agente pide permiso para algo.
    PermissionRequest,
    /// El agente va a usar una tool.
    PreToolUse,
}

impl HookKind {
    /// Mapea el nombre de evento del provider a nuestro tipo normalizado.
    pub fn from_event_name(name: &str) -> Option<Self> {
        match name.trim() {
            "Stop" | "SubagentStop" | "stop" => Some(Self::Stop),
            "UserPromptSubmit" | "user_prompt_submit" => Some(Self::UserPromptSubmit),
            "PermissionRequest" | "Notification" | "permission_request" => {
                Some(Self::PermissionRequest)
            }
            "PreToolUse" | "pre_tool_use" => Some(Self::PreToolUse),
            _ => None,
        }
    }
}

/// Evento de hook ya normalizado, listo para el orquestador.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookEvent {
    pub provider: String,
    pub kind: HookKind,
    /// Panel que originó el evento, si el hook lo reportó (`TC_PANEL_ID`).
    pub panel_id: Option<Uuid>,
    pub workspace_id: Option<Uuid>,
    /// Id de sesión del agente, para el resume exacto.
    pub session_id: Option<String>,
}

/// Params de query que mandan los hooks (vienen de las env vars del spawn).
#[derive(Debug, Default, Deserialize)]
pub struct HookQuery {
    pub panel: Option<String>,
    pub workspace: Option<String>,
}

/// Construye el evento desde el payload crudo del provider. Puro y testeable:
/// el body de Claude trae `hook_event_name` y `session_id`.
pub fn parse_hook_payload(
    provider: &str,
    body: &serde_json::Value,
    query: &HookQuery,
) -> Option<HookEvent> {
    let event_name = body
        .get("hook_event_name")
        .and_then(|value| value.as_str())
        .or_else(|| body.get("event").and_then(|value| value.as_str()))?;
    let kind = HookKind::from_event_name(event_name)?;
    let session_id = body
        .get("session_id")
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .filter(|id| !id.trim().is_empty());
    Some(HookEvent {
        provider: provider.to_owned(),
        kind,
        panel_id: query
            .panel
            .as_deref()
            .and_then(|id| Uuid::parse_str(id).ok()),
        workspace_id: query
            .workspace
            .as_deref()
            .and_then(|id| Uuid::parse_str(id).ok()),
        session_id,
    })
}

/// Comparación de tokens en tiempo constante-ish: no filtra el largo por
/// early-return byte a byte.
pub fn token_matches(expected: &str, provided: Option<&str>) -> bool {
    let Some(provided) = provided else {
        return false;
    };
    if expected.len() != provided.len() {
        return false;
    }
    expected
        .bytes()
        .zip(provided.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// Directorio donde vive el endpoint file.
pub fn hooks_dir() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "terminal-app")?;
    Some(dirs.data_dir().join("agent-hooks"))
}

/// Contenido del endpoint file que sourcean los hooks.
pub fn endpoint_script(url: &str, token: &str) -> String {
    format!("export TC_HOOK_URL={url}\nexport TC_HOOK_TOKEN={token}\n")
}

/// Captura de un elemento del navegador mandada por la extensión (P3.18).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesignCapture {
    /// Selector legible del elemento (`div.card > button`).
    pub selector: String,
    pub html: String,
    pub css: String,
    /// Rect en la página, como lo reportó `getBoundingClientRect`.
    pub rect: String,
    /// Screenshot recortado ya escrito a disco, si vino.
    pub screenshot_path: Option<std::path::PathBuf>,
}

/// Payload crudo que manda la extensión.
#[derive(Debug, Default, Deserialize)]
pub struct DesignPayload {
    #[serde(default)]
    pub selector: String,
    #[serde(default)]
    pub html: String,
    #[serde(default)]
    pub css: String,
    #[serde(default)]
    pub rect: serde_json::Value,
    /// PNG en base64 (con o sin el prefijo `data:image/png;base64,`).
    #[serde(default)]
    pub screenshot_b64: Option<String>,
}

/// Tope del HTML/CSS que aceptamos: una página entera no entra en un prompt y
/// tampoco queremos que un `outerHTML` de 20 MB nos coma la memoria.
pub const MAX_CAPTURE_FIELD: usize = 32 * 1024;

fn clamp_field(text: &str) -> String {
    if text.len() <= MAX_CAPTURE_FIELD {
        return text.to_owned();
    }
    let mut end = MAX_CAPTURE_FIELD;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Rect como texto compacto; si no vino con la forma esperada, queda vacío.
fn rect_summary(rect: &serde_json::Value) -> String {
    let number = |key: &str| rect.get(key).and_then(serde_json::Value::as_f64);
    match (number("x"), number("y"), number("width"), number("height")) {
        (Some(x), Some(y), Some(width), Some(height)) => {
            format!("x={x:.0} y={y:.0} w={width:.0} h={height:.0}")
        }
        _ => String::new(),
    }
}

/// Decodifica el screenshot y lo escribe a disco. `None` si no vino o si el
/// base64 es inválido (la captura sigue siendo útil sin imagen).
pub fn write_screenshot(base64_png: &str, dir: &std::path::Path) -> Option<std::path::PathBuf> {
    use base64::Engine;
    let payload = base64_png
        .split_once("base64,")
        .map(|(_, rest)| rest)
        .unwrap_or(base64_png);
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .ok()?;
    if bytes.is_empty() {
        return None;
    }
    std::fs::create_dir_all(dir).ok()?;
    let path = dir.join(format!("design-{}.png", Uuid::new_v4().simple()));
    std::fs::write(&path, bytes).ok()?;
    Some(path)
}

/// Arma la captura desde el payload, recortando campos gigantes.
pub fn build_design_capture(
    payload: DesignPayload,
    screenshot_path: Option<std::path::PathBuf>,
) -> DesignCapture {
    DesignCapture {
        selector: clamp_field(payload.selector.trim()),
        html: clamp_field(payload.html.trim()),
        css: clamp_field(payload.css.trim()),
        rect: rect_summary(&payload.rect),
        screenshot_path,
    }
}

/// Formato determinístico del prompt que se le manda al agente (P3.18, T3).
/// Byte-exacto: el agente puede parsearlo sin ambigüedad.
pub fn format_design_capture(capture: &DesignCapture) -> String {
    let mut out = String::with_capacity(capture.html.len() + capture.css.len() + 128);
    out.push_str(&format!(
        "Element: {}
",
        capture.selector
    ));
    if !capture.rect.is_empty() {
        out.push_str(&format!(
            "Rect: {}
",
            capture.rect
        ));
    }
    out.push_str(&format!(
        "HTML: {}
",
        capture.html
    ));
    out.push_str(&format!(
        "CSS: {}
",
        capture.css
    ));
    if let Some(path) = capture.screenshot_path.as_ref() {
        out.push_str(&format!(
            "Screenshot: {}
",
            path.display()
        ));
    }
    out
}

#[derive(Clone)]
struct HookState {
    token: String,
    sender: Arc<Mutex<Sender<HookEvent>>>,
    design_sender: Arc<Mutex<Sender<DesignCapture>>>,
    design_dir: std::path::PathBuf,
}

async fn hook_handler(
    State(state): State<HookState>,
    AxumPath(provider): AxumPath<String>,
    Query(query): Query<HookQuery>,
    headers: HeaderMap,
    body: String,
) -> StatusCode {
    let provided = headers
        .get(TOKEN_HEADER)
        .and_then(|value| value.to_str().ok());
    if !token_matches(&state.token, provided) {
        return StatusCode::UNAUTHORIZED;
    }
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(event) = parse_hook_payload(&provider, &json, &query) else {
        // Evento que no nos interesa: 204, no es un error del hook.
        return StatusCode::NO_CONTENT;
    };
    if let Ok(sender) = state.sender.lock() {
        let _ = sender.send(event);
    }
    StatusCode::OK
}

async fn design_handler(
    State(state): State<HookState>,
    headers: HeaderMap,
    body: String,
) -> StatusCode {
    let provided = headers
        .get(TOKEN_HEADER)
        .and_then(|value| value.to_str().ok());
    if !token_matches(&state.token, provided) {
        return StatusCode::UNAUTHORIZED;
    }
    let Ok(payload) = serde_json::from_str::<DesignPayload>(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    let screenshot = payload
        .screenshot_b64
        .as_deref()
        .and_then(|encoded| write_screenshot(encoded, &state.design_dir));
    let capture = build_design_capture(payload, screenshot);
    if let Ok(sender) = state.design_sender.lock() {
        let _ = sender.send(capture);
    }
    StatusCode::OK
}

/// Servidor de hooks vivo. Al dropearse, el hilo queda cerrado por el cierre
/// del listener del runtime.
pub struct HookServer {
    url: String,
    token: String,
    receiver: Receiver<HookEvent>,
    design_receiver: Receiver<DesignCapture>,
    _thread: thread::JoinHandle<()>,
}

impl HookServer {
    /// Levanta el servidor en un puerto efímero de loopback y escribe el
    /// endpoint file.
    pub fn start() -> anyhow::Result<Self> {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let url = format!("http://127.0.0.1:{port}");
        let token = Uuid::new_v4().simple().to_string();

        let (sender, receiver) = std::sync::mpsc::channel::<HookEvent>();
        let (design_sender, design_receiver) = std::sync::mpsc::channel::<DesignCapture>();
        let design_dir = hooks_dir()
            .map(|dir| dir.join("captures"))
            .unwrap_or_else(std::env::temp_dir);
        let state = HookState {
            token: token.clone(),
            sender: Arc::new(Mutex::new(sender)),
            design_sender: Arc::new(Mutex::new(design_sender)),
            design_dir,
        };
        let router = Router::new()
            .route("/hook/:provider", post(hook_handler))
            .route("/design/capture", post(design_handler))
            .with_state(state);

        // Nada de unwrap acá: un fallo del server de hooks no puede tirar la app.
        let thread_handle = thread::spawn(move || {
            let runtime = match tokio::runtime::Runtime::new() {
                Ok(runtime) => runtime,
                Err(err) => {
                    log::error!("no se pudo crear el runtime del hook server: {err}");
                    return;
                }
            };
            runtime.block_on(async move {
                let listener = match tokio::net::TcpListener::from_std(listener) {
                    Ok(listener) => listener,
                    Err(err) => {
                        log::error!("no se pudo adoptar el listener del hook server: {err}");
                        return;
                    }
                };
                if let Err(err) = axum::serve(listener, router).await {
                    log::error!("el hook server se detuvo con error: {err}");
                }
            });
        });

        let server = Self {
            url,
            token,
            receiver,
            design_receiver,
            _thread: thread_handle,
        };
        server.write_endpoint_file();
        Ok(server)
    }

    fn write_endpoint_file(&self) {
        let Some(dir) = hooks_dir() else {
            return;
        };
        if let Err(err) = std::fs::create_dir_all(&dir) {
            log::warn!("no se pudo crear el dir de hooks: {err}");
            return;
        }
        let path = dir.join("endpoint.sh");
        if let Err(err) = std::fs::write(&path, endpoint_script(&self.url, &self.token)) {
            log::warn!("no se pudo escribir el endpoint file de hooks: {err}");
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    /// Drena los eventos recibidos desde el último frame.
    pub fn poll(&self) -> Vec<HookEvent> {
        let mut out = Vec::new();
        while let Ok(event) = self.receiver.try_recv() {
            out.push(event);
        }
        out
    }

    /// Drena las capturas de Design Mode recibidas desde el último frame.
    pub fn poll_design(&self) -> Vec<DesignCapture> {
        let mut out = Vec::new();
        while let Ok(capture) = self.design_receiver.try_recv() {
            out.push(capture);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::{
        endpoint_script, parse_hook_payload, token_matches, HookKind, HookQuery, HookServer,
    };
    use uuid::Uuid;

    fn query(panel: Option<&str>) -> HookQuery {
        HookQuery {
            panel: panel.map(str::to_owned),
            workspace: None,
        }
    }

    #[test]
    fn event_names_map_to_normalized_kinds() {
        assert_eq!(HookKind::from_event_name("Stop"), Some(HookKind::Stop));
        assert_eq!(
            HookKind::from_event_name("SubagentStop"),
            Some(HookKind::Stop)
        );
        assert_eq!(
            HookKind::from_event_name("UserPromptSubmit"),
            Some(HookKind::UserPromptSubmit)
        );
        assert_eq!(
            HookKind::from_event_name("PermissionRequest"),
            Some(HookKind::PermissionRequest)
        );
        assert_eq!(
            HookKind::from_event_name("PreToolUse"),
            Some(HookKind::PreToolUse)
        );
        assert_eq!(HookKind::from_event_name("Whatever"), None);
    }

    #[test]
    fn payload_extracts_kind_session_and_panel() {
        let panel = Uuid::new_v4();
        let body = serde_json::json!({
            "hook_event_name": "Stop",
            "session_id": "abc-123",
        });
        let event = parse_hook_payload("claude", &body, &query(Some(&panel.to_string())))
            .expect("evento válido");
        assert_eq!(event.kind, HookKind::Stop);
        assert_eq!(event.session_id.as_deref(), Some("abc-123"));
        assert_eq!(event.panel_id, Some(panel));
        assert_eq!(event.provider, "claude");
    }

    #[test]
    fn payload_without_a_known_event_is_ignored() {
        let body = serde_json::json!({ "hook_event_name": "Nope" });
        assert!(parse_hook_payload("claude", &body, &query(None)).is_none());
    }

    #[test]
    fn payload_without_event_name_is_ignored() {
        let body = serde_json::json!({ "session_id": "x" });
        assert!(parse_hook_payload("claude", &body, &query(None)).is_none());
    }

    #[test]
    fn blank_session_id_is_dropped() {
        let body = serde_json::json!({ "hook_event_name": "Stop", "session_id": "   " });
        let event = parse_hook_payload("claude", &body, &query(None)).expect("evento");
        assert_eq!(event.session_id, None);
    }

    #[test]
    fn a_bad_panel_id_does_not_break_the_event() {
        let body = serde_json::json!({ "hook_event_name": "Stop" });
        let event =
            parse_hook_payload("claude", &body, &query(Some("no-es-uuid"))).expect("evento");
        assert_eq!(event.panel_id, None);
    }

    #[test]
    fn tokens_must_match_exactly() {
        assert!(token_matches("abc", Some("abc")));
        assert!(!token_matches("abc", Some("abd")));
        assert!(!token_matches("abc", Some("abcd")), "distinto largo");
        assert!(!token_matches("abc", None), "sin header no pasa");
    }

    #[test]
    fn endpoint_script_exports_url_and_token() {
        let script = endpoint_script("http://127.0.0.1:9999", "tok");
        assert!(script.contains("export TC_HOOK_URL=http://127.0.0.1:9999"));
        assert!(script.contains("export TC_HOOK_TOKEN=tok"));
    }

    #[test]
    fn server_binds_an_ephemeral_loopback_port() {
        let server = HookServer::start().expect("arranca");
        assert!(
            server.url().starts_with("http://127.0.0.1:"),
            "{}",
            server.url()
        );
        assert!(!server.token().is_empty());
        // Sin eventos todavía.
        assert!(server.poll().is_empty());
    }

    #[test]
    fn the_design_prompt_is_byte_exact() {
        let capture = super::DesignCapture {
            selector: "div.card > button".to_owned(),
            html: "<button>Ok</button>".to_owned(),
            css: "color: red;".to_owned(),
            rect: "x=10 y=20 w=100 h=40".to_owned(),
            screenshot_path: Some(std::path::PathBuf::from("/tmp/design-1.png")),
        };
        assert_eq!(
            super::format_design_capture(&capture),
            "Element: div.card > button\nRect: x=10 y=20 w=100 h=40\nHTML: <button>Ok</button>\nCSS: color: red;\nScreenshot: /tmp/design-1.png\n"
        );
    }

    #[test]
    fn the_prompt_omits_missing_optional_lines() {
        let capture = super::DesignCapture {
            selector: "button".to_owned(),
            html: "<button/>".to_owned(),
            css: String::new(),
            rect: String::new(),
            screenshot_path: None,
        };
        let prompt = super::format_design_capture(&capture);
        assert!(!prompt.contains("Rect:"), "got {prompt:?}");
        assert!(!prompt.contains("Screenshot:"), "got {prompt:?}");
        assert!(prompt.starts_with("Element: button\n"));
    }

    #[test]
    fn giant_fields_are_clamped_without_splitting_a_character() {
        let payload = super::DesignPayload {
            selector: "div".to_owned(),
            html: "ñ".repeat(super::MAX_CAPTURE_FIELD),
            css: String::new(),
            rect: serde_json::Value::Null,
            screenshot_b64: None,
        };
        let capture = super::build_design_capture(payload, None);
        assert!(capture.html.len() <= super::MAX_CAPTURE_FIELD + 4);
        assert!(capture.html.ends_with('…'), "se marca el recorte");
    }

    #[test]
    fn the_rect_summary_survives_a_payload_without_it() {
        let payload = super::DesignPayload {
            selector: "div".to_owned(),
            html: "<div/>".to_owned(),
            css: String::new(),
            rect: serde_json::json!({"nope": 1}),
            screenshot_b64: None,
        };
        assert_eq!(super::build_design_capture(payload, None).rect, "");
    }

    #[test]
    fn the_rect_summary_formats_the_expected_shape() {
        let payload = super::DesignPayload {
            selector: "div".to_owned(),
            html: String::new(),
            css: String::new(),
            rect: serde_json::json!({"x": 10.4, "y": 20.6, "width": 100.0, "height": 40.0}),
            screenshot_b64: None,
        };
        assert_eq!(
            super::build_design_capture(payload, None).rect,
            "x=10 y=21 w=100 h=40"
        );
    }

    #[test]
    fn a_screenshot_is_written_with_or_without_the_data_url_prefix() {
        use base64::Engine;
        let dir = std::env::temp_dir().join(format!("design-{}", Uuid::new_v4()));
        let png = b"\x89PNG\r\n\x1a\nfake";
        let encoded = base64::engine::general_purpose::STANDARD.encode(png);

        let plain = super::write_screenshot(&encoded, &dir).expect("escribe");
        assert_eq!(std::fs::read(&plain).unwrap(), png);

        let with_prefix =
            super::write_screenshot(&format!("data:image/png;base64,{encoded}"), &dir)
                .expect("escribe");
        assert_eq!(std::fs::read(&with_prefix).unwrap(), png);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_screenshot_does_not_kill_the_capture() {
        let dir = std::env::temp_dir().join(format!("design-bad-{}", Uuid::new_v4()));
        assert_eq!(super::write_screenshot("no-es-base64!!!", &dir), None);
        assert_eq!(super::write_screenshot("", &dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
