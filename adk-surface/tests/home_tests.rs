//! The web home's page (spec 3.3 §5.1, §10 S1): `adk-surface`'s `web` feature serves the
//! embedded app at `/ui/`, each section as the same page, under a strict content security
//! policy, and answers `404` for a file it does not ship.

#![cfg(feature = "web")]

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt;

async fn get(uri: &str) -> (StatusCode, axum::http::HeaderMap, String) {
    let response = adk_surface::web::home()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test]
async fn the_root_and_bare_ui_redirect_to_the_home() {
    for uri in ["/", "/ui"] {
        let (status, headers, _) = get(uri).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{uri}");
        assert_eq!(headers[header::LOCATION], adk_surface::web::HOME_PATH, "{uri}");
    }
}

#[tokio::test]
async fn every_section_is_the_page_under_a_strict_policy() {
    for uri in ["/ui/", "/ui/today", "/ui/memory", "/ui/developer"] {
        let (status, headers, body) = get(uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert!(headers[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/html"), "{uri}");
        assert_eq!(headers[header::CACHE_CONTROL], "no-cache", "{uri}");
        let policy = headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
        for directive in ["default-src 'self'", "script-src 'self'", "frame-ancestors 'none'"] {
            assert!(policy.contains(directive), "{uri}: {policy}");
        }
        assert!(body.contains(r#"<div id="root">"#), "{uri}");
        // The page names its script and styles under `/ui/assets/`, which this router serves.
        assert!(body.contains(r#"src="/ui/assets/"#), "{uri}");
    }
}

/// Every `/ui/assets/…` path in `text`, in order.
fn asset_paths(text: &str) -> Vec<String> {
    text.match_indices("/ui/assets/")
        .map(|(start, _)| {
            let rest = &text[start..];
            let end =
                rest.find(|c: char| matches!(c, '"' | '\'' | ')' | ' ')).unwrap_or(rest.len());
            rest[..end].to_string()
        })
        .collect()
}

/// Every file the page names — its script and stylesheet — and every file the stylesheet
/// names, its fonts, is served, so a build whose new files were never committed fails here.
#[tokio::test]
async fn every_asset_the_page_names_is_served_immutable() {
    let (_, _, page) = get("/ui/").await;
    let named = asset_paths(&page);
    let script =
        named.iter().find(|path| path.ends_with(".js")).expect("the page names its script");
    let style =
        named.iter().find(|path| path.ends_with(".css")).expect("the page names its styles");
    let (status, _, css) = get(style).await;
    assert_eq!(status, StatusCode::OK, "{style}");
    let fonts = asset_paths(&css);
    assert!(fonts.iter().any(|path| path.ends_with(".woff2")), "the styles name their fonts");
    for path in named.iter().chain(&fonts) {
        let (status, headers, _) = get(path).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(headers[header::CACHE_CONTROL], "public, max-age=31536000, immutable", "{path}");
        assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff", "{path}");
        assert!(headers.get(header::CONTENT_SECURITY_POLICY).is_none(), "{path}");
    }
    let (_, headers, _) = get(script).await;
    assert!(headers[header::CONTENT_TYPE].to_str().unwrap().contains("javascript"));
}

#[tokio::test]
async fn a_file_the_home_does_not_ship_is_not_found() {
    for uri in ["/ui/assets/missing.js", "/ui/fixture.html", "/ui/today/missing.png"] {
        let (status, _, _) = get(uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
}
