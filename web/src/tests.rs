use super::*;
use axum::http::Request as HttpRequest;
use tower::ServiceExt;

fn state() -> WebState {
    WebState {
        hash: make_hash(b"local-test-password-123").unwrap(),
        origin: "http://localhost:8900".into(),
        secure: false,
        sessions: Default::default(),
        attempts: Default::default(),
        verification_slots: Arc::new(Semaphore::new(4)),
    }
}

fn request(method: &str, path: &str, session: Option<&str>, body: &str) -> HttpRequest<Body> {
    let mut builder = HttpRequest::builder()
        .method(method)
        .uri(path)
        .header("host", "localhost:8900")
        .header("origin", "http://localhost:8900");
    if let Some(session) = session {
        builder = builder.header("cookie", session);
    }
    let content_type = if path == "/login" {
        "application/x-www-form-urlencoded"
    } else {
        "application/json"
    };
    let mut request = builder
        .header("content-type", content_type)
        .body(Body::from(body.to_string()))
        .unwrap();
    request.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:12345".parse::<SocketAddr>().unwrap(),
    ));
    request
}

#[tokio::test]
async fn every_private_entry_is_gated() {
    let state = state();
    let app = router(state, std::path::Path::new("../dist"));
    for path in [
        "/",
        "/index.html",
        "/assets/app.js",
        "/media/img?u=x",
        "/media/hls?u=x",
        "/media/seg?u=x",
        "/.env",
        "/anything",
    ] {
        let response = app
            .clone()
            .oneshot(request("GET", path, None, ""))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER, "{path}");
        assert_eq!(response.headers()["location"], "/login");
        assert_eq!(response.headers()["cache-control"], "private, no-store");
    }
    let response = app
        .oneshot(request("POST", "/api/invoke/api_feed", None, "{}"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn login_logout_and_cookie_revocation() {
    let state = state();
    let app = router(state.clone(), std::path::Path::new("../dist"));
    let page = app
        .clone()
        .oneshot(request("GET", "/login", None, ""))
        .await
        .unwrap();
    assert_eq!(page.headers()["referrer-policy"], "same-origin");
    let wrong = app
        .clone()
        .oneshot(request("POST", "/login", None, "password=incorrect"))
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    assert!(!wrong.headers().contains_key("set-cookie"));
    let good = app
        .clone()
        .oneshot(request(
            "POST",
            "/login",
            None,
            "password=local-test-password-123",
        ))
        .await
        .unwrap();
    assert_eq!(good.status(), StatusCode::SEE_OTHER);
    let set_cookie = good.headers()["set-cookie"].to_str().unwrap();
    assert!(set_cookie.contains("HttpOnly") && set_cookie.contains("SameSite=Strict"));
    let cookie = set_cookie.split(';').next().unwrap();
    let response = app
        .clone()
        .oneshot(request("POST", "/api/invoke/app_boot", Some(cookie), "{}"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/invoke/plugin:app|exit",
            Some(cookie),
            "{}",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = app
        .clone()
        .oneshot(request("POST", "/logout", Some(cookie), ""))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let response = app
        .oneshot(request("POST", "/api/invoke/app_boot", Some(cookie), "{}"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(state.sessions.lock().unwrap().is_empty());
}

#[tokio::test]
async fn csrf_and_unexpected_hosts_are_rejected() {
    let app = router(state(), std::path::Path::new("../dist"));
    for origin in [None, Some("https://other.example")] {
        let mut req = request("POST", "/login", None, "password=local-test-password-123");
        req.headers_mut().remove("origin");
        if let Some(origin) = origin {
            req.headers_mut().insert("origin", origin.parse().unwrap());
        }
        assert_eq!(
            app.clone().oneshot(req).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
    let mut req = request("GET", "/login", None, "");
    req.headers_mut()
        .insert("host", "attacker.example".parse().unwrap());
    assert_eq!(
        app.oneshot(req).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn failed_login_attempts_are_limited() {
    let state = state();
    state
        .attempts
        .lock()
        .unwrap()
        .insert("127.0.0.1".parse().unwrap(), (Instant::now(), 10));
    let app = router(state, std::path::Path::new("../dist"));
    assert_eq!(
        app.oneshot(request(
            "POST",
            "/login",
            None,
            "password=local-test-password-123"
        ))
        .await
        .unwrap()
        .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[test]
fn expired_sessions_do_not_authenticate_and_https_cookies_are_secure() {
    let mut state = state();
    state.secure = true;
    assert!(state.cookie_header("token", 100).ends_with("; Secure"));
    state.sessions.lock().unwrap().insert(
        "expired".into(),
        BrowserSession {
            expires: Instant::now() - Duration::from_secs(1),
            api: Arc::new(api::Api::new()),
        },
    );
    let mut headers = HeaderMap::new();
    headers.insert("cookie", "hghub_session=expired".parse().unwrap());
    assert!(state.session(&headers).is_none());
}

#[test]
fn upstream_accounts_are_isolated_per_browser_session() {
    let a = api::Api::new();
    let b = api::Api::new();
    a.session
        .set(net::DEFAULT_BASE, "private-token".into(), json!({"id":1}));
    assert!(a.session.token(net::DEFAULT_BASE).is_some());
    assert!(b.session.token(net::DEFAULT_BASE).is_none());
}
