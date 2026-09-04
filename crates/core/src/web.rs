//! The built web client, served with the cache headers a hashed-asset bundle needs.

use std::path::Path;

use axum::{
    Router,
    extract::Request,
    http::{HeaderValue, header},
    middleware::{self, Next},
    response::Response,
};
use tower_http::services::{ServeDir, ServeFile};

/// A year — the longest `max-age` that means anything — plus `immutable`, so a reload does not
/// even revalidate.
const FOREVER: &str = "public, max-age=31536000, immutable";

/// Serve `dir` as the single-page app: unknown paths fall back to `index.html` so `#/v/<id>`
/// links work, and every response says how long it may be reused.
pub fn client(dir: &Path) -> Router {
    let files = ServeDir::new(dir).fallback(ServeFile::new(dir.join("index.html")));
    Router::new().fallback_service(files).layer(middleware::from_fn(cache_control))
}

/// Vite names everything under `/assets/` by the hash of its contents, so those bytes are the
/// same forever and a browser never needs to ask again. Everything else keeps its name across
/// deploys and must be revalidated — the document above all: with no header saying so, a
/// browser is free to invent a freshness lifetime from the file's age and go on serving an
/// `index.html` that names the *previous* bundle, which makes a finished deploy invisible.
/// `no-cache` is not "do not store": the copy is kept and confirmed with an `ETag`, so the
/// usual answer is still a 304.
async fn cache_control(req: Request, next: Next) -> Response {
    let hashed = req.uri().path().starts_with("/assets/");
    let mut res = next.run(req).await;
    let value = if hashed { FOREVER } else { "no-cache" };
    res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static(value));
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn serve(dir: &Path) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = client(dir);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    /// (status, cache-control) for a GET.
    async fn get(url: String) -> (u16, String) {
        tokio::task::spawn_blocking(move || {
            let r = ureq::get(&url).call().unwrap();
            let cc = r.headers().get("cache-control").map(|v| v.to_str().unwrap().to_owned());
            (r.status().as_u16(), cc.unwrap_or_default())
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn the_document_revalidates_and_the_hashed_assets_do_not() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "<!doctype html><title>x</title>").unwrap();
        std::fs::write(dir.path().join("sw.js"), "// worker").unwrap();
        std::fs::create_dir(dir.path().join("assets")).unwrap();
        std::fs::write(dir.path().join("assets/index-abc123.js").as_path(), "console.log(1)").unwrap();
        let base = serve(dir.path()).await;

        assert_eq!(get(format!("{base}/")).await, (200, "no-cache".to_owned()));
        // The SPA fallback is the document too, however it was reached.
        assert_eq!(get(format!("{base}/some/route")).await, (200, "no-cache".to_owned()));
        // The service worker keeps its name across deploys: cache it and the app cannot update.
        assert_eq!(get(format!("{base}/sw.js")).await, (200, "no-cache".to_owned()));
        assert_eq!(get(format!("{base}/assets/index-abc123.js")).await, (200, FOREVER.to_owned()));
    }
}
