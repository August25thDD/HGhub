//! Personal, password-gated HTTP host for the same Rust core used by Tauri.
#[path = "../../src-tauri/src/api.rs"]
mod api;
#[path = "../../src-tauri/src/lines.rs"]
mod lines;
#[path = "../../src-tauri/src/logging.rs"]
mod logging;
#[path = "../../src-tauri/src/media.rs"]
mod media;
#[path = "../../src-tauri/src/net.rs"]
mod net;
#[path = "../../src-tauri/src/network_policy.rs"]
mod network_policy;
#[path = "../../src-tauri/src/playback_log.rs"]
mod playback_log;
#[path = "../../src-tauri/src/session.rs"]
mod session;
#[cfg(test)]
mod tests;

use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    body::Body,
    extract::{ConnectInfo, DefaultBodyLimit, Extension, Path, RawQuery, Request, State},
    http::{header, HeaderMap, Method, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand_core::{OsRng, RngCore};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::{self, Read},
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
use tower_http::services::ServeDir;

const SESSION_TTL: Duration = Duration::from_secs(7 * 86400);
const COOKIE: &str = "hghub_session";
const LOGIN: &str = include_str!("../login.html");

struct BrowserSession {
    expires: Instant,
    api: Arc<api::Api>,
}
#[derive(Clone)]
struct WebState {
    username: String,
    hash: String,
    origin: String,
    secure: bool,
    sessions: Arc<Mutex<HashMap<String, BrowserSession>>>,
    attempts: Arc<Mutex<HashMap<IpAddr, (Instant, u32)>>>,
    verification_slots: Arc<Semaphore>,
}

fn token() -> String {
    let mut bytes = [0; 32];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(name, value)| (name == COOKIE).then_some(value))
}

impl WebState {
    fn session(&self, headers: &HeaderMap) -> Option<Arc<api::Api>> {
        let id = cookie(headers)?;
        let mut sessions = self.sessions.lock().ok()?;
        sessions.retain(|_, session| session.expires > Instant::now());
        sessions.get(id).map(|session| session.api.clone())
    }

    fn cookie_header(&self, value: &str, age: u32) -> String {
        format!(
            "{COOKIE}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={age}{}",
            if self.secure { "; Secure" } else { "" }
        )
    }
}

async fn boundaries(State(state): State<WebState>, request: Request, next: Next) -> Response {
    // Do not trust forwarding headers: public origin is explicitly configured.
    let expected_host = state.origin.split("://").nth(1).unwrap_or("");
    if request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        != Some(expected_host)
    {
        return (StatusCode::BAD_REQUEST, "unexpected host").into_response();
    }
    if !matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    ) {
        let origin = request
            .headers()
            .get(header::ORIGIN)
            .and_then(|h| h.to_str().ok());
        if origin != Some(state.origin.as_str()) {
            return (StatusCode::FORBIDDEN, "same-origin request required").into_response();
        }
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, "private, no-store".parse().unwrap());
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    headers.insert(header::X_FRAME_OPTIONS, "DENY".parse().unwrap());
    // A no-referrer policy turns browser form POST origins into "null".
    // same-origin keeps the login/logout CSRF check usable, without leaking paths cross-site.
    headers.insert("referrer-policy", "same-origin".parse().unwrap());
    response
}

async fn authenticated(
    State(state): State<WebState>,
    mut request: Request,
    next: Next,
) -> Response {
    match state.session(request.headers()) {
        Some(api) => {
            request.extensions_mut().insert(api);
            next.run(request).await
        }
        None if request.uri().path().starts_with("/api/") => {
            (StatusCode::UNAUTHORIZED, Json(json!({"error":"请先登录"}))).into_response()
        }
        None => Redirect::to("/login").into_response(),
    }
}

async fn login_page(State(state): State<WebState>, headers: HeaderMap) -> Response {
    if state.session(&headers).is_some() {
        return Redirect::to("/").into_response();
    }
    Html(LOGIN.replace("{{ERROR}}", "")).into_response()
}

#[derive(Deserialize)]
struct LoginForm {
    username: String,
    password: String,
}

async fn login(
    State(state): State<WebState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Form(form): Form<LoginForm>,
) -> Response {
    let allowed = {
        let mut attempts = state.attempts.lock().unwrap();
        attempts.retain(|_, (since, _)| since.elapsed() < Duration::from_secs(900));
        if attempts.len() >= 1024 && !attempts.contains_key(&peer.ip()) {
            false
        } else {
            let entry = attempts.entry(peer.ip()).or_insert((Instant::now(), 0));
            entry.1 += 1;
            entry.1 <= 10
        }
    };
    if !allowed {
        return (StatusCode::TOO_MANY_REQUESTS, "尝试过多，请稍后再试").into_response();
    }
    let Ok(permit) = state.verification_slots.clone().try_acquire_owned() else {
        return (StatusCode::TOO_MANY_REQUESTS, "请稍后再试").into_response();
    };
    let hash = state.hash.clone();
    let valid = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        PasswordHash::new(&hash).is_ok_and(|parsed| {
            Argon2::default()
                .verify_password(form.password.as_bytes(), &parsed)
                .is_ok()
        })
    })
    .await
    .unwrap_or(false);
    if !valid || form.username != state.username {
        return (
            StatusCode::UNAUTHORIZED,
            Html(LOGIN.replace("{{ERROR}}", "账号或密码不正确")),
        )
            .into_response();
    }
    let id = token();
    {
        let mut sessions = state.sessions.lock().unwrap();
        sessions.retain(|_, session| session.expires > Instant::now());
        if sessions.len() >= 128 {
            return (StatusCode::SERVICE_UNAVAILABLE, "登录会话已满").into_response();
        }
        sessions.insert(
            id.clone(),
            BrowserSession {
                expires: Instant::now() + SESSION_TTL,
                api: Arc::new(api::Api::new()),
            },
        );
    }
    state.attempts.lock().unwrap().remove(&peer.ip());
    let mut response = Redirect::to("/").into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        state
            .cookie_header(&id, SESSION_TTL.as_secs() as u32)
            .parse()
            .unwrap(),
    );
    response
}

async fn logout(State(state): State<WebState>, headers: HeaderMap) -> Response {
    if let Some(id) = cookie(&headers) {
        state.sessions.lock().unwrap().remove(id);
    }
    let mut response = Redirect::to("/login").into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        state.cookie_header("", 0).parse().unwrap(),
    );
    response
}

async fn proxy(Path(kind): Path<String>, RawQuery(query): RawQuery) -> Response {
    if !matches!(kind.as_str(), "img" | "hls" | "seg") {
        return StatusCode::NOT_FOUND.into_response();
    }
    // Root-relative URLs work both on iPhone Safari and behind a reverse proxy.
    let response = media::handle(
        &format!("/{kind}"),
        query.as_deref().unwrap_or(""),
        "/media",
    )
    .await;
    response.map(Body::from)
}

fn field<T: serde::de::DeserializeOwned>(args: &Value, name: &str) -> Result<T, String> {
    serde_json::from_value(args.get(name).cloned().unwrap_or(Value::Null))
        .map_err(|_| format!("参数无效: {name}"))
}
fn page(args: &Value) -> u32 {
    args["page"].as_u64().unwrap_or(1).clamp(1, 10000) as u32
}
fn size(args: &Value) -> u32 {
    args["size"].as_u64().unwrap_or(20).clamp(1, 100) as u32
}
fn enc(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

async fn dispatch(cmd: &str, a: Value, api: &api::Api) -> Result<Value, String> {
    // An explicit command allowlist, never arbitrary upstream paths or native plugin calls.
    Ok(match cmd {
        "app_boot" => api::app_boot(api).await?,
        "api_feed" => api::api_feed(api, page(&a), size(&a)).await?,
        "api_category" => {
            api::api_category(api, enc(&field::<String>(&a, "cate")?), page(&a), size(&a)).await?
        }
        "api_rank" => api::api_rank(api, field(&a, "kind")?).await?,
        "api_search" => api::api_search(api, field(&a, "q")?, page(&a), size(&a)).await?,
        "api_posts" => api::api_posts(api, page(&a), size(&a)).await?,
        "api_post" => api::api_post(api, field(&a, "id")?).await?,
        "api_video" => api::api_video(api, field(&a, "id")?).await?,
        "api_play" => api::api_play(api, field(&a, "id")?, field(&a, "ep")?).await?,
        "api_comments" => {
            api::api_comments(api, field(&a, "id")?, field(&a, "ep")?, page(&a), size(&a)).await?
        }
        "api_post_comments" => {
            api::api_post_comments(api, field(&a, "id")?, page(&a), size(&a)).await?
        }
        "api_login" => api::api_login(api, field(&a, "username")?, field(&a, "password")?).await?,
        "api_register" => {
            api::api_register(
                api,
                field(&a, "username")?,
                field(&a, "password")?,
                a["email"].as_str().unwrap_or("").to_string(),
            )
            .await?
        }
        "api_logout" => api::api_logout(api).await?,
        "api_me" => api::api_me(api).await?,
        "api_like" => {
            api::api_like(api, field(&a, "id")?, field(&a, "ep")?, field(&a, "on")?).await?
        }
        "api_collect" => api::api_collect(api, field(&a, "id")?, field(&a, "on")?).await?,
        "api_follow" => api::api_follow(api, field(&a, "uid")?, field(&a, "on")?).await?,
        "api_post_like" => api::api_post_like(api, field(&a, "id")?, field(&a, "on")?).await?,
        "api_post_collect" => {
            api::api_post_collect(api, field(&a, "id")?, field(&a, "on")?).await?
        }
        "api_comment_send" => {
            api::api_comment_send(
                api,
                field(&a, "id")?,
                field(&a, "ep")?,
                field(&a, "text")?,
                field(&a, "replyId")?,
            )
            .await?
        }
        "line_state" => api::line_state().await?,
        "line_set" => json!(api::line_set(api, field(&a, "url")?).await?),
        "line_mirrors" => api::line_mirrors().await?,
        "line_probe" => {
            let urls: Vec<String> = field(&a, "urls")?;
            if urls.len() > 16 {
                return Err("一次最多检测 16 条线路".into());
            }
            api::line_probe(urls).await?
        }
        "cache_clear" => {
            api::cache_clear().await?;
            Value::Null
        }
        // Server network policy stays strict even if a browser toggles the desktop setting.
        "security_set" => {
            if !field::<bool>(&a, "strict")? {
                return Err("网页版保持严格网络保护".into());
            }
            json!(true)
        }
        "playback_log_status" => {
            serde_json::to_value(playback_log::playback_log_status()?).map_err(|e| e.to_string())?
        }
        "playback_log_set" => {
            serde_json::to_value(playback_log::playback_log_set(field(&a, "enabled")?)?)
                .map_err(|e| e.to_string())?
        }
        "playback_log_write" => {
            playback_log::playback_log_write(field(&a, "entries")?)?;
            Value::Null
        }
        "playback_log_read" => json!(playback_log::playback_log_read()?),
        "check_update" => api::check_update().await?,
        // Do not write arbitrary client-supplied messages (they may contain media URLs).
        "debug_log" => Value::Null,
        "net_diag" => json!({"strict_security":true,"platform":"web","status":"server managed"}),
        _ => return Err("不支持的命令".into()),
    })
}

async fn invoke(
    Path(cmd): Path<String>,
    Extension(api): Extension<Arc<api::Api>>,
    Json(args): Json<Value>,
) -> Response {
    match dispatch(&cmd, args, &api).await {
        Ok(result) => Json(result).into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, Json(json!({"error":error}))).into_response(),
    }
}

fn router(state: WebState, dist: &std::path::Path) -> Router {
    let private = Router::new()
        .route("/api/invoke/{cmd}", post(invoke))
        .route("/media/{kind}", get(proxy))
        .route("/logout", post(logout))
        .fallback_service(ServeDir::new(dist))
        .layer(middleware::from_fn_with_state(state.clone(), authenticated));
    Router::new()
        .route("/login", get(login_page).post(login))
        .merge(private)
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), boundaries))
        .with_state(state)
}

fn make_hash(password: &[u8]) -> Result<String, String> {
    Argon2::default()
        .hash_password(password, &SaltString::generate(&mut OsRng))
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().any(|a| a == "--hash-password") {
        let mut password = String::new();
        io::stdin().take(4096).read_to_string(&mut password)?;
        let password = password.trim_end_matches(['\r', '\n']);
        if password.len() < 16 {
            return Err("password must contain at least 16 bytes".into());
        }
        println!("{}", make_hash(password.as_bytes())?);
        return Ok(());
    }
    if network_policy::cli_strict_override() == Some(false) {
        return Err("web server requires strict network policy".into());
    }
    let listen: SocketAddr = std::env::var("HGHUB_LISTEN")
        .unwrap_or_else(|_| "127.0.0.1:8900".into())
        .parse()?;
    let origin =
        std::env::var("HGHUB_PUBLIC_ORIGIN").unwrap_or_else(|_| format!("http://{listen}"));
    let url = url::Url::parse(&origin)?;
    if url.origin().ascii_serialization() != origin
        || !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(
            "HGHUB_PUBLIC_ORIGIN must be an HTTP(S) origin without path or credentials".into(),
        );
    }
    let hash_file = std::env::var("HGHUB_PASSWORD_HASH_FILE")?;
    let username = std::env::var("HGHUB_USERNAME")?;
    if username.trim().is_empty() || username.len() > 128 {
        return Err("HGHUB_USERNAME must contain 1 to 128 bytes".into());
    }
    let hash = std::fs::read_to_string(hash_file)?.trim().to_owned();
    PasswordHash::new(&hash).map_err(|_| "invalid password hash")?;
    let data = PathBuf::from(std::env::var("HGHUB_DATA_DIR").unwrap_or_else(|_| "web-data".into()));
    std::fs::create_dir_all(&data)?;
    net::bind_base(&data);
    network_policy::set_strict_security(true)?;
    playback_log::bind(&data);
    let dist = PathBuf::from(std::env::var("HGHUB_DIST").unwrap_or_else(|_| "dist".into()));
    if !dist.join("index.html").is_file() {
        return Err("web build dist/index.html is missing".into());
    }
    let state = WebState {
        username,
        hash,
        secure: url.scheme() == "https",
        origin,
        sessions: Default::default(),
        attempts: Default::default(),
        verification_slots: Arc::new(Semaphore::new(4)),
    };
    let listener = tokio::net::TcpListener::bind(listen).await?;
    println!(
        "HGhub Rust HTTP service listening on {}",
        listener.local_addr()?
    );
    axum::serve(
        listener,
        router(state, &dist).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    Ok(())
}
