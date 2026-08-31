use std::{env, net::SocketAddr, sync::Arc};

use axum::{
    extract::{DefaultBodyLimit, Multipart, Request, State},
    http::{header::AUTHORIZATION, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::json;
use subtle::ConstantTimeEq;
use tower_http::{limit::RequestBodyLimitLayer, trace::TraceLayer};
use tracing::info;

const DEFAULT_MAX_AUDIO_BYTES: usize = 256 * 1024 * 1024;

#[derive(Clone)]
struct AppState {
    client: reqwest::Client,
    qwen_url: String,
    qwen_model: String,
    api_key: String,
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    temperature: u8,
}

#[derive(Serialize)]
struct Message {
    role: &'static str,
    content: Vec<AudioPart>,
}

#[derive(Serialize)]
struct AudioPart {
    #[serde(rename = "type")]
    kind: &'static str,
    input_audio: InputAudio,
}

#[derive(Serialize)]
struct InputAudio {
    data: String,
    format: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: AssistantMessage,
}

#[derive(Deserialize)]
struct AssistantMessage {
    content: String,
}

#[derive(Serialize)]
struct TranscriptionResponse {
    text: String,
}

struct ApiError {
    status: StatusCode,
    message: String,
    error_type: &'static str,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
            error_type: "invalid_request_error",
        }
    }

    fn upstream(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            message: message.into(),
            error_type: "api_error",
        }
    }

    fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: "Invalid API key".to_owned(),
            error_type: "authentication_error",
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({
                "error": {
                    "message": self.message,
                    "type": self.error_type
                }
            })),
        )
            .into_response()
    }
}

async fn health() -> StatusCode {
    StatusCode::OK
}

async fn transcribe(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<Json<TranscriptionResponse>, ApiError> {
    let mut audio = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| ApiError::bad_request(format!("invalid multipart request: {error}")))?
    {
        if field.name() != Some("file") {
            continue;
        }

        let format = audio_format(field.file_name(), field.content_type().map(|mime| mime.as_ref()));
        let bytes = field
            .bytes()
            .await
            .map_err(|error| ApiError::bad_request(format!("failed to read audio: {error}")))?;
        audio = Some((STANDARD.encode(bytes), format));
        break;
    }

    let Some((data, format)) = audio else {
        return Err(ApiError::bad_request("missing multipart field: file"));
    };

    let request = ChatRequest {
        model: state.qwen_model.clone(),
        messages: vec![Message {
            role: "user",
            content: vec![AudioPart {
                kind: "input_audio",
                input_audio: InputAudio { data, format },
            }],
        }],
        temperature: 0,
    };

    let response = state
        .client
        .post(&state.qwen_url)
        .json(&request)
        .send()
        .await
        .map_err(|error| ApiError::upstream(format!("Qwen request failed: {error}")))?
        .error_for_status()
        .map_err(|error| ApiError::upstream(format!("Qwen returned an error: {error}")))?
        .json::<ChatResponse>()
        .await
        .map_err(|error| ApiError::upstream(format!("invalid Qwen response: {error}")))?;

    let content = response
        .choices
        .first()
        .ok_or_else(|| ApiError::upstream("Qwen returned no transcription"))?
        .message
        .content
        .trim();

    // Qwen's native output starts with "language <detected><asr_text>".
    let text = content
        .split_once("<asr_text>")
        .map(|(_, text)| text)
        .unwrap_or(content)
        .trim()
        .to_owned();

    Ok(Json(TranscriptionResponse { text }))
}

async fn require_auth(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    authorize(request.headers(), &state.api_key)?;
    Ok(next.run(request).await)
}

fn authorize(headers: &HeaderMap, api_key: &str) -> Result<(), ApiError> {
    let expected = format!("Bearer {api_key}");
    let authorized = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(|value| bool::from(value.as_bytes().ct_eq(expected.as_bytes())))
        .unwrap_or(false);

    authorized.then_some(()).ok_or_else(ApiError::unauthorized)
}

fn audio_format(filename: Option<&str>, mime: Option<&str>) -> String {
    let extension = filename
        .and_then(|name| name.rsplit_once('.').map(|(_, extension)| extension))
        .map(str::to_ascii_lowercase);

    match extension.as_deref().or(mime) {
        Some("wav") | Some("audio/wav") | Some("audio/x-wav") => "wav",
        Some("mp3") | Some("audio/mpeg") => "mp3",
        Some("m4a") | Some("audio/mp4") => "m4a",
        Some("flac") | Some("audio/flac") => "flac",
        Some("ogg") | Some("audio/ogg") => "ogg",
        Some("webm") | Some("audio/webm") => "webm",
        _ => "wav",
    }
    .to_owned()
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let max_audio_bytes = env::var("MAX_AUDIO_BYTES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_MAX_AUDIO_BYTES);
    let state = Arc::new(AppState {
        client: reqwest::Client::new(),
        qwen_url: env::var("QWEN_URL")
            .unwrap_or_else(|_| "http://qwen3-asr:8000/v1/chat/completions".to_owned()),
        qwen_model: env::var("QWEN_MODEL").unwrap_or_else(|_| "qwen3-asr-1.7b".to_owned()),
        api_key: env::var("LLAMA_API_KEY").expect("LLAMA_API_KEY must be set"),
    });

    let protected_routes = Router::new()
        .route("/v1/audio/transcriptions", post(transcribe))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth));

    let app = Router::new()
        .route("/health", get(health))
        .merge(protected_routes)
        .with_state(state)
        .layer(RequestBodyLimitLayer::new(max_audio_bytes))
        .layer(DefaultBodyLimit::max(max_audio_bytes))
        .layer(TraceLayer::new_for_http());

    let address = SocketAddr::from(([0, 0, 0, 0], 8080));
    info!(%address, max_audio_bytes, "starting ASR proxy");
    axum::serve(tokio::net::TcpListener::bind(address).await.unwrap(), app)
        .await
        .unwrap();
}
