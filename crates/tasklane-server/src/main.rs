use axum::{response::Html, routing::get, Router};

pub fn app() -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/", get(console))
}

async fn console() -> Html<&'static str> {
    Html("<!doctype html><title>tasklane</title><h1>tasklane</h1><p>Console placeholder. Design phase: nothing runs yet.</p>")
}

#[tokio::main]
async fn main() {
    let bind = std::env::var("TASKLANE_BIND").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let listener = tokio::net::TcpListener::bind(&bind).await.expect("bind");
    eprintln!("tasklane-server listening on {bind}");
    axum::serve(listener, app()).await.expect("serve");
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn healthz_says_ok() {
        let res = app()
            .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"ok");
    }
}
