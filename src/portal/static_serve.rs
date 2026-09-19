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
    // Walk the embedded tree component by component.
    let mut node: &Dir = &WEB_DIST;
    let parts: Vec<&str> = path.split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        if i == parts.len() - 1 {
            return node.get_entry(part);
        }
        node = node.get_dir(part)?;
    }
    None
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
    }
}
