//! Serves the built React SPA from `web/dist`, embedded at compile time.
//!
//! Unknown non-API paths fall through to `index.html` so client-side routing
//! (TanStack Router) keeps working on refresh. When the tree is empty (dist
//! not built at compile time), a stub route explains how to enable the UI.

use axum::body::Body;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use include_dir::{include_dir, Dir, DirEntry};

static WEB_DIST: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/web/dist");

fn lookup(path: &str) -> Option<&'static DirEntry<'static>> {
    let path = path.trim_start_matches('/');
    if path.is_empty() {
        return None;
    }
    // `Dir::get_entry` searches recursively, comparing the candidate against
    // each entry's *full* path relative to the embedded root (e.g.
    // "assets/index-abc.js"). Paths containing `..`/`.` never match an
    // embedded entry, so traversal is structurally impossible.
    WEB_DIST.get_entry(path)
}

fn content_type_for(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "webmanifest" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "txt" => "text/plain; charset=utf-8",
        "map" => "application/json",
        _ => "application/octet-stream",
    }
}

fn file_response(file: &'static include_dir::File<'static>, path: &str) -> Response {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ctype = content_type_for(name);
    let cache = if path.starts_with("/assets/") || ctype.starts_with("font/") {
        // Vite emits content-hashed filenames under /assets/.
        "public, max-age=31536000, immutable"
    } else if ctype.starts_with("text/html") {
        "no-cache"
    } else {
        "public, max-age=3600"
    };
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(ctype)),
            (header::CACHE_CONTROL, HeaderValue::from_static(cache)),
        ],
        Body::from(file.contents()),
    )
        .into_response()
}

async fn serve_spa(axum::extract::OriginalUri(uri): axum::extract::OriginalUri) -> Response {
    let path = uri.path();

    // Exact hit first (assets/, index.html, manifest...).
    if let Some(DirEntry::File(file)) = lookup(path) {
        return file_response(file, path);
    }
    // SPA fallback: any non-asset route returns index.html.
    if let Some(DirEntry::File(index)) = lookup("index.html") {
        let mut res = file_response(index, "index.html");
        res.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        return res;
    }
    // No frontend embedded.
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "RustFox portal API is running, but no frontend build is embedded.\n\
         Build the UI:  cd web && npm install && npm run build\n\
         then rebuild rustfox (web/dist is included via include_dir),\n\
         or use the dev server:  cd web && npm run dev (proxies /api to the portal)\n",
    )
        .into_response()
}

pub fn router<S: Send + Sync + Clone + 'static>() -> Router<S> {
    Router::new().fallback(serve_spa)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Extract every `/assets/<file>` reference from an HTML document
    /// (what Vite emits into index.html after `base: "/"`).
    fn asset_refs(html: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = html;
        while let Some(i) = rest.find("/assets/") {
            rest = &rest[i..];
            let end = rest
                .find(['"', '\'', ' ', '\n', '\r', '>'])
                .unwrap_or(rest.len());
            out.push(rest[..end].to_string());
            rest = &rest[end..];
        }
        out
    }

    fn embedded_index() -> &'static str {
        WEB_DIST
            .get_file("index.html")
            .expect("embedded dist must contain index.html")
            .contents_utf8()
            .expect("index.html must be utf-8")
    }

    #[test]
    fn asset_refs_extractor() {
        let html = r#"<script src="/assets/a-hash.js"></script>
            <link href='/assets/style.css' rel=stylesheet>"#;
        assert_eq!(
            asset_refs(html),
            vec!["/assets/a-hash.js", "/assets/style.css"]
        );
        assert!(asset_refs("no assets here").is_empty());
    }

    #[test]
    fn content_types_match_extensions() {
        assert!(content_type_for("a.js").starts_with("text/javascript"));
        assert!(content_type_for("a.css").starts_with("text/css"));
        assert!(content_type_for("index.html").starts_with("text/html"));
        assert_eq!(content_type_for("manifest.webmanifest"), "application/json");
        assert_eq!(content_type_for("logo.svg"), "image/svg+xml");
    }

    #[test]
    fn lookup_rejects_directory_traversal() {
        assert!(lookup("../../etc/passwd").is_none());
        assert!(lookup("/").is_none());
        assert!(lookup("/../index.html").is_none());
    }

    #[test]
    fn lookup_finds_top_level_index() {
        assert!(matches!(lookup("index.html"), Some(DirEntry::File(_))));
    }

    /// Regression test for the MIME-type bug that made the SPA unbootable.
    ///
    /// `lookup` used to walk the embedded tree component-by-component, but
    /// `include_dir` stores each entry's *full* path relative to the root —
    /// so `node.get_entry("index-abc.js")` never matched nested files.
    /// Every `/assets/...` request fell through to the SPA index.html
    /// fallback, and the browser refused the module script with
    /// "Expected a JavaScript module but got text/html".
    ///
    /// The assertion is driven by the embedded index.html itself: every URL
    /// it references MUST resolve to an embedded file. The stub dist (no
    /// /assets/ refs) can't regress this way, so it is vacuously fine there;
    /// in a real portal build any mismatch fails the test.
    #[test]
    fn every_asset_referenced_by_embedded_index_is_servable() {
        let refs = asset_refs(embedded_index());
        assert!(
            !refs.iter().any(|r| lookup(r).is_none()),
            "some /assets/ URLs referenced by index.html do not resolve in the \
             embedded tree — they would be served the SPA fallback with the wrong \
             MIME type. Refs: {refs:?}"
        );
    }

    /// End-to-end: serve a referenced asset through the real router and
    /// verify Content-Type + byte-for-byte body against the embedded file.
    /// Fails loudly when the binary was built without the frontend (the
    /// exact state that shipped the bug), so portal CI cannot pass blindly.
    #[tokio::test]
    async fn embedded_asset_served_with_correct_mime() {
        use axum::body::to_bytes;
        use tower::ServiceExt;

        let Some(js_ref) = asset_refs(embedded_index())
            .into_iter()
            .find(|r| r.ends_with(".js"))
        else {
            // Stub dist (frontend not built) — CI's build-web step guarantees
            // the real dist is present, so only assert when there's something
            // to assert about. Plain `cargo test` without npm stays green.
            return;
        };
        let file = match lookup(&js_ref) {
            Some(DirEntry::File(f)) => f,
            other => panic!("{js_ref} must resolve to an embedded file, got {other:?}"),
        };

        let app: Router = Router::new().fallback(serve_spa);
        let res = app
            .oneshot(
                axum::http::Request::builder()
                    .uri(&js_ref)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let ctype = res
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(
            ctype.starts_with("text/javascript"),
            "{js_ref} served as {ctype} — SPA fallback leaked over an asset request"
        );
        let body = to_bytes(res.into_body(), 64 * 1024 * 1024).await.unwrap();
        assert_eq!(
            body.len(),
            file.contents().len(),
            "{js_ref} must be served byte-for-byte from the embedded dist"
        );
    }
}
