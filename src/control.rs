//! HTTP control plane (axum). The Soli app (or the test page) exchanges SDP
//! and group updates here; media never touches HTTP. In production this sits
//! behind soli-proxy for TLS - it is a tiny, low-traffic API.
//!
//!   POST   /v1/sessions          {token, sdp_offer, peers?} -> {session_id, sdp_answer}
//!   PATCH  /v1/sessions/:id      {token, peers}             -> 204
//!   DELETE /v1/sessions/:id      {token}                    -> 204
//!   GET    /v1/stats                                        -> {sessions, rooms}
//!   GET    /healthz                                         -> ok

use crate::auth;
use crate::config::Config;
use crate::engine::{Cmd, StatsSnapshot};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tower_http::cors::{Any, CorsLayer};

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub cmd_tx: mpsc::Sender<Cmd>,
}

pub fn router(state: AppState) -> Router {
    // The browser calls this API cross-origin from the bonfire origin; SDP
    // exchange carries its own HMAC token, so a permissive CORS is fine.
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/stats", get(stats))
        .route("/v1/sessions", post(create_session))
        .route(
            "/v1/sessions/{id}",
            axum::routing::patch(set_peers).delete(drop_session),
        )
        .route("/v1/sessions/{id}/slots", post(slots))
        .layer(cors)
        .with_state(state)
}

// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CreateReq {
    token: String,
    sdp_offer: String,
    /// user_ids this subscriber should hear; omitted = everyone in the room.
    peers: Option<Vec<String>>,
}

#[derive(Serialize)]
struct CreateRes {
    session_id: u64,
    sdp_answer: String,
}

#[derive(Deserialize)]
struct PeersReq {
    token: String,
    peers: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct DropReq {
    token: String,
}

fn err(status: StatusCode, msg: &str) -> Response {
    (status, Json(serde_json::json!({ "error": msg }))).into_response()
}

fn authenticate(state: &AppState, token: &str) -> Option<auth::Claims> {
    auth::verify(token, &state.cfg.secret, state.cfg.allow_unauthenticated)
}

async fn create_session(State(state): State<AppState>, Json(req): Json<CreateReq>) -> Response {
    let Some(claims) = authenticate(&state, &req.token) else {
        return err(StatusCode::UNAUTHORIZED, "invalid token");
    };
    let (reply, rx) = oneshot::channel();
    let cmd = Cmd::NewSession {
        claims,
        offer_sdp: req.sdp_offer,
        peers: req.peers,
        reply,
    };
    if state.cmd_tx.send(cmd).await.is_err() {
        return err(StatusCode::SERVICE_UNAVAILABLE, "engine down");
    }
    match rx.await {
        Ok(Ok(s)) => Json(CreateRes {
            session_id: s.session_id,
            sdp_answer: s.answer_sdp,
        })
        .into_response(),
        Ok(Err(e)) => err(StatusCode::BAD_REQUEST, &e.to_string()),
        Err(_) => err(StatusCode::SERVICE_UNAVAILABLE, "engine down"),
    }
}

async fn set_peers(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    Json(req): Json<PeersReq>,
) -> Response {
    let Some(claims) = authenticate(&state, &req.token) else {
        return err(StatusCode::UNAUTHORIZED, "invalid token");
    };
    let (reply, rx) = oneshot::channel();
    let cmd = Cmd::SetPeers {
        session_id: id,
        user_id: claims.user_id,
        peers: req.peers,
        reply,
    };
    if state.cmd_tx.send(cmd).await.is_err() {
        return err(StatusCode::SERVICE_UNAVAILABLE, "engine down");
    }
    match rx.await {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) => err(StatusCode::NOT_FOUND, &e.to_string()),
        Err(_) => err(StatusCode::SERVICE_UNAVAILABLE, "engine down"),
    }
}

async fn drop_session(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    Json(req): Json<DropReq>,
) -> Response {
    let Some(claims) = authenticate(&state, &req.token) else {
        return err(StatusCode::UNAUTHORIZED, "invalid token");
    };
    let (reply, rx) = oneshot::channel();
    let cmd = Cmd::Drop {
        session_id: id,
        user_id: claims.user_id,
        reply,
    };
    if state.cmd_tx.send(cmd).await.is_err() {
        return err(StatusCode::SERVICE_UNAVAILABLE, "engine down");
    }
    match rx.await {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) => err(StatusCode::NOT_FOUND, &e.to_string()),
        Err(_) => err(StatusCode::SERVICE_UNAVAILABLE, "engine down"),
    }
}

/// Current slot bindings of MY session: {slot_mid: publisher_user_id}.
/// POST (not GET) so the token rides the body like every other route.
async fn slots(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    Json(req): Json<DropReq>,
) -> Response {
    let Some(claims) = authenticate(&state, &req.token) else {
        return err(StatusCode::UNAUTHORIZED, "invalid token");
    };
    let (reply, rx) = oneshot::channel();
    let cmd = Cmd::Slots {
        session_id: id,
        user_id: claims.user_id,
        reply,
    };
    if state.cmd_tx.send(cmd).await.is_err() {
        return err(StatusCode::SERVICE_UNAVAILABLE, "engine down");
    }
    match rx.await {
        Ok(Ok(map)) => Json(map).into_response(),
        Ok(Err(e)) => err(StatusCode::NOT_FOUND, &e.to_string()),
        Err(_) => err(StatusCode::SERVICE_UNAVAILABLE, "engine down"),
    }
}

async fn stats(State(state): State<AppState>) -> Response {
    let (reply, rx) = oneshot::channel();
    if state.cmd_tx.send(Cmd::Stats { reply }).await.is_err() {
        return err(StatusCode::SERVICE_UNAVAILABLE, "engine down");
    }
    match rx.await {
        Ok(snap) => Json::<StatsSnapshot>(snap).into_response(),
        Err(_) => err(StatusCode::SERVICE_UNAVAILABLE, "engine down"),
    }
}
