//! Browser boundary for the local daemon, including mutation and checkpoint routes.
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, Method, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use tower_http::cors::{AllowOrigin, Any, CorsLayer};

use crate::graph::AppState;

/// The authority of an `http(s)://host[:port]` origin with nothing after it.
pub(crate) fn origin_authority(origin: &str) -> Option<&str> {
    let authority = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))?;
    if authority.is_empty() || authority.contains(['/', '?', '#', '@']) {
        return None;
    }
    authority.parse::<axum::http::uri::Authority>().ok()?;
    Some(authority)
}

fn address_authority(authority: &str) -> bool {
    let Ok(authority) = authority.parse::<axum::http::uri::Authority>() else {
        return false;
    };
    let host = authority
        .host()
        .trim_start_matches('[')
        .trim_end_matches(']');
    !authority.as_str().contains('@')
        && (host.eq_ignore_ascii_case("localhost") || host.parse::<std::net::IpAddr>().is_ok())
}

fn trusted(headers: &HeaderMap, uri: &Uri, allowed: &[String]) -> bool {
    // Never let duplicate or undecodable headers fall through as absent.
    for name in [
        header::HOST.as_str(),
        header::ORIGIN.as_str(),
        "sec-fetch-site",
    ] {
        if headers.get_all(name).iter().count() > 1
            || headers.get(name).is_some_and(|v| v.to_str().is_err())
        {
            return false;
        }
    }
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .or_else(|| uri.authority().map(|a| a.as_str()));
    // Same-origin equality alone permits DNS rebinding. Enforce this even
    // when Origin is absent, as on a browser's same-origin GET. A hostname
    // the operator listed as an allowed origin is theirs to trust: that is how
    // a daemon exposed on a network interface is reached by name.
    if host.is_some_and(|host| {
        !address_authority(host) && !allowed.iter().any(|o| origin_authority(o) == Some(host))
    }) {
        return false;
    }
    match headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        Some(origin) => origin_authority(origin).is_some_and(|authority| {
            host.is_some_and(|host| authority == host || allowed.iter().any(|o| o == origin))
        }),
        None => {
            // Cross-site GETs (images/navigation/no-cors fetch) can omit Origin
            // but still trigger handlers. Reject them even on read-only routes.
            !headers
                .get("sec-fetch-site")
                .is_some_and(|value| value != "same-origin" && value != "none")
        }
    }
}

pub(super) async fn guard_request(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    if !trusted(request.headers(), request.uri(), &state.allowed_origins) {
        return (
            StatusCode::FORBIDDEN,
            "untrusted browser origin or daemon host",
        )
            .into_response();
    }
    next.run(request).await
}

pub(super) fn cors_layer(allowed: Vec<String>) -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(move |_, parts| {
            trusted(&parts.headers, &parts.uri, &allowed)
        }))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers(Any)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(
        host: Option<&str>,
        origin: Option<&str>,
        site: Option<&str>,
        allowed: &[String],
    ) -> bool {
        let mut headers = HeaderMap::new();
        for (name, value) in [("host", host), ("origin", origin), ("sec-fetch-site", site)] {
            if let Some(value) = value {
                headers.insert(name, value.parse().unwrap());
            }
        }
        trusted(
            &headers,
            &"/api/v1/harness/checkpoint".parse().unwrap(),
            allowed,
        )
    }

    #[test]
    fn browser_policy_blocks_forgery_and_dns_rebinding() {
        for origin in [
            "https://evil.example",
            "null",
            "http://localhost:3000",
            "http://127.0.0.1:7474/path",
        ] {
            assert!(
                !check(Some("127.0.0.1:7474"), Some(origin), None, &[]),
                "{origin}"
            );
        }
        assert!(!check(
            Some("rebind.example:7474"),
            Some("http://rebind.example:7474"),
            None,
            &[]
        ));
        assert!(!check(Some("rebind.example:7474"), None, None, &[]));
        assert!(!check(
            Some("127.0.0.1:7474"),
            None,
            Some("cross-site"),
            &[]
        ));
        assert!(!check(Some("127.0.0.1:7474"), None, Some("same-site"), &[]));
        assert!(!check(None, Some("http://127.0.0.1:7474"), None, &[]));
    }

    #[test]
    fn native_same_origin_and_explicit_development_origins_work() {
        assert!(check(Some("127.0.0.1:7474"), None, None, &[]));
        assert!(check(None, None, None, &[])); // In-process native clients.
        for host in [
            "localhost:7474",
            "127.0.0.1:7474",
            "[::1]:7474",
            "192.168.1.5:7474",
        ] {
            assert!(check(
                Some(host),
                Some(&format!("http://{host}")),
                Some("same-origin"),
                &[]
            ));
        }
        // A daemon exposed on the network, reached by the name its operator
        // listed: the page load (no Origin) and its same-origin fetches.
        let named = ["http://mbp.local:7480".to_string()];
        assert!(check(Some("mbp.local:7480"), None, None, &named));
        assert!(check(
            Some("mbp.local:7480"),
            Some("http://mbp.local:7480"),
            Some("same-origin"),
            &named
        ));
        assert!(!check(Some("mbp.local:7480"), None, None, &[]));
        assert!(!check(Some("rebind.example:7480"), None, None, &named));
        assert!(!check(
            Some("mbp.local:7480"),
            Some("http://rebind.example:7480"),
            None,
            &named
        ));
        assert!(check(
            Some("127.0.0.1:7474"),
            Some("http://localhost:5173"),
            Some("cross-site"),
            &["http://localhost:5173".into()]
        ));
        assert!(!check(
            Some("evil.example:7474"),
            Some("http://localhost:5173"),
            None,
            &["http://localhost:5173".into()]
        ));
    }
}
