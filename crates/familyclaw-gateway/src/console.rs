//! Browser-based, operator-safe reliability console routes.
//!
//! The console only renders data that has already been redacted by the
//! approval and turn-audit surfaces. It intentionally has no build step or
//! external assets, so it remains available with the gateway alone.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use futures_util::stream;
use serde::Deserialize;

use crate::{constant_time_eq, GatewayState};

/// Query parameters accepted by the `EventSource` endpoint.
#[derive(Deserialize)]
pub(super) struct ConsoleEventsQuery {
    /// Optional bearer token for browsers, whose `EventSource` API cannot set headers.
    token: Option<String>,
}

/// Serves the self-contained Reliability Console page.
///
/// The HTML shell itself is unauthenticated on purpose: it contains no secrets
/// and needs to load so the browser can prompt for a bearer token. Protected
/// data arrives only through [`console_events`] and the existing approval /
/// audit JSON routes (header or `?token=` for `EventSource`).
pub(super) async fn console_page() -> Response {
    Html(include_str!("console.html")).into_response()
}

/// Streams redacted turn-audit events as Server-Sent Events.
///
/// The collector is polled once per second. Events are append-only, so the
/// stream keeps an insertion index and emits each newly observed event exactly
/// once for this client connection. The optional query token is compared in
/// constant time and is never logged.
pub(super) async fn console_events(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    Query(query): Query<ConsoleEventsQuery>,
) -> Response {
    if !console_events_authorized(&state, &headers, query.token.as_deref()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let Some(audit) = state.turn_audit.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let audit = Arc::clone(audit);
    let events = stream::unfold(
        (audit, 0_usize, VecDeque::new()),
        |(audit, mut sent, mut queued)| async move {
            while queued.is_empty() {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let collected = audit.list();
                if collected.len() < sent {
                    // The collector is normally append-only. Resetting is a
                    // safe fallback if a future implementation rotates it.
                    sent = 0;
                }
                let collected_len = collected.len();
                queued.extend(collected.into_iter().skip(sent));
                sent = collected_len;
            }

            let event = queued.pop_front();
            event.map(|event| {
                let data = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
                (
                    Ok::<Event, Infallible>(Event::default().data(data)),
                    (audit, sent, queued),
                )
            })
        },
    );

    Sse::new(events)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Validates header-or-query bearer authentication for the SSE endpoint.
fn console_events_authorized(
    state: &GatewayState,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> bool {
    let Some(expected) = state.inject_token.as_deref() else {
        return true;
    };
    let header_token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim);

    header_token.is_some_and(|token| constant_time_eq(token.as_bytes(), expected.as_bytes()))
        || query_token.is_some_and(|token| constant_time_eq(token.as_bytes(), expected.as_bytes()))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::{to_bytes, Body};
    use axum::http::{header, Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn console_page_returns_html_without_token() {
        // The shell is public so the browser can prompt for a token; secrets
        // stay behind SSE + JSON routes.
        let mut state = crate::test_gateway_state();
        state.inject_token = Some(Arc::from("test-token"));
        let app = crate::build_router(Arc::new(state));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/console")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");

        assert_eq!(response.status(), StatusCode::OK);
        assert!(response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/html")));
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body reads");
        assert!(String::from_utf8_lossy(&body).contains("Reliability Console"));
        assert!(String::from_utf8_lossy(&body).contains("id=\"now\""));
    }

    #[tokio::test]
    async fn console_events_require_bearer_or_query_token_when_configured() {
        let mut state = crate::test_gateway_state();
        state.inject_token = Some(Arc::from("test-token"));
        let app = crate::build_router(Arc::new(state));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/console/events")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
