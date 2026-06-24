use axum::http::{StatusCode, Uri};
use axum::response::IntoResponse;

#[cfg(all(feature = "embedded-frontend", not(feature = "headless")))]
use axum::http::header;
#[cfg(all(feature = "embedded-frontend", not(feature = "headless")))]
use axum::response::{Html, Response};
#[cfg(all(feature = "embedded-frontend", not(feature = "headless")))]
use include_dir::{include_dir, Dir};

#[cfg(all(feature = "embedded-frontend", not(feature = "headless")))]
static STATIC_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/ui/dist");

#[cfg(all(feature = "embedded-frontend", not(feature = "headless")))]
pub async fn serve_static(uri: Uri) -> impl IntoResponse {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    if let Some(file) = STATIC_DIR.get_file(path) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime.as_ref())
            .body(file.contents().into())
            .expect("static response");
    }

    if !path.starts_with("api/") {
        if let Some(index) = STATIC_DIR.get_file("index.html") {
            return Html(std::str::from_utf8(index.contents()).unwrap_or("")).into_response();
        }
    }

    (StatusCode::NOT_FOUND, "Not Found").into_response()
}

#[cfg(any(not(feature = "embedded-frontend"), feature = "headless"))]
pub async fn serve_static(_uri: Uri) -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        "MOOSEDev UI is not embedded in this build",
    )
}
