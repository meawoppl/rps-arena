//! Curl-friendly REST polling API over the match engine.
//!
//! An HTTP "player" is just a different transport feeding the same mpsc
//! channels the WebSocket path uses — `run_match`/matchmaker are unchanged.
//! Flow: `register` → `queue` → poll `poll` and react (on `RoundStart` POST
//! `commit`, on `AwaitReveal` POST `reveal`), optionally `chat`.
//!
//! Auth: the `register` token is passed as `Authorization: Bearer <token>`
//! (preferred) or `?token=<token>`. The token is a secret — it controls the
//! player.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    extract::{Query, State},
    http::{header::AUTHORIZATION, HeaderMap, StatusCode},
    Json,
};
use serde::Deserialize;
use tokio::sync::{mpsc, Mutex, Notify};
use uuid::Uuid;

use shared::{
    ClientMsg, PlayChatRequest, PlayCommitRequest, PlayError, PlayOk, PlayPollResponse,
    PlayQueueRequest, PlayRegisterRequest, PlayRegisterResponse, PlayRevealRequest, ServerMsg,
};

use crate::game::{self, PlayerConn};
use crate::{db_ops, AppState};

/// Max buffered server messages per HTTP session before we cut it loose.
const OUTBOX_CAP: usize = 512;
/// Idle session TTL (no requests for this long → reaped).
const IDLE_TTL: Duration = Duration::from_secs(600);
/// Max chat length accepted over HTTP.
const MAX_CHAT_LEN: usize = 2000;

/// Server-side state for one HTTP player.
pub struct HttpSession {
    agent_id: Uuid,
    player_id: Uuid,
    model: String,
    display_name: String,
    test: bool,
    /// Sender into the engine (filled by POST commit/reveal/chat).
    in_tx: mpsc::UnboundedSender<ClientMsg>,
    /// Receiver + outbound sender, taken when the player joins the queue.
    in_rx: Option<mpsc::UnboundedReceiver<ClientMsg>>,
    out_tx: Option<mpsc::UnboundedSender<ServerMsg>>,
    /// Buffered server→player messages, drained by `poll`.
    outbox: Arc<Mutex<VecDeque<ServerMsg>>>,
    notify: Arc<Notify>,
    committed: HashSet<Uuid>,
    revealed: HashSet<Uuid>,
    queued: bool,
    last_seen: Instant,
}

pub type HttpSessions = Arc<Mutex<HashMap<Uuid, HttpSession>>>;

pub fn new_sessions() -> HttpSessions {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Periodically reap idle sessions so abandoned registrations don't leak.
pub fn spawn_reaper(sessions: HttpSessions) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            sessions
                .lock()
                .await
                .retain(|_, s| s.last_seen.elapsed() < IDLE_TTL);
        }
    });
}

#[derive(Debug, Deserialize)]
pub struct TokenQuery {
    token: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct PollQuery {
    token: Option<Uuid>,
    timeout_ms: Option<u64>,
    limit: Option<usize>,
}

fn err(code: StatusCode, msg: &str) -> (StatusCode, Json<PlayError>) {
    (code, Json(PlayError { error: msg.into() }))
}

/// Token from `Authorization: Bearer <token>` (preferred) or `?token=`.
fn token_of(headers: &HeaderMap, query: Option<Uuid>) -> Option<Uuid> {
    if let Some(val) = headers.get(AUTHORIZATION).and_then(|h| h.to_str().ok()) {
        if let Some(t) = val
            .strip_prefix("Bearer ")
            .or_else(|| val.strip_prefix("bearer "))
        {
            if let Ok(u) = Uuid::parse_str(t.trim()) {
                return Some(u);
            }
        }
    }
    query
}

async fn drain(outbox: &Arc<Mutex<VecDeque<ServerMsg>>>, limit: usize) -> Vec<ServerMsg> {
    let mut ob = outbox.lock().await;
    let n = ob.len().min(limit);
    ob.drain(..n).collect()
}

pub async fn register(
    State(app): State<Arc<AppState>>,
    Json(req): Json<PlayRegisterRequest>,
) -> Result<Json<PlayRegisterResponse>, (StatusCode, Json<PlayError>)> {
    if req.model.trim().is_empty() || req.display_name.trim().is_empty() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "model and display_name required",
        ));
    }
    let agent_id = Uuid::new_v4();
    let player_id = Uuid::new_v4();
    if let Err(e) = db_ops::create_player(
        &app.db_pool,
        player_id,
        req.model.clone(),
        req.display_name.clone(),
        req.test,
    )
    .await
    {
        tracing::warn!("create_player failed: {e}");
    }

    let (in_tx, in_rx) = mpsc::unbounded_channel::<ClientMsg>();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<ServerMsg>();
    let outbox = Arc::new(Mutex::new(VecDeque::new()));
    let notify = Arc::new(Notify::new());
    let token = Uuid::new_v4();

    // Drain engine output into the outbox; cut the session loose on overflow.
    {
        let outbox = outbox.clone();
        let notify = notify.clone();
        let sessions = app.http_sessions.clone();
        tokio::spawn(async move {
            while let Some(m) = out_rx.recv().await {
                let mut ob = outbox.lock().await;
                if ob.len() >= OUTBOX_CAP {
                    drop(ob);
                    sessions.lock().await.remove(&token);
                    break;
                }
                ob.push_back(m);
                drop(ob);
                notify.notify_one();
            }
        });
    }

    app.http_sessions.lock().await.insert(
        token,
        HttpSession {
            agent_id,
            player_id,
            model: req.model,
            display_name: req.display_name,
            test: req.test,
            in_tx,
            in_rx: Some(in_rx),
            out_tx: Some(out_tx),
            outbox,
            notify,
            committed: HashSet::new(),
            revealed: HashSet::new(),
            queued: false,
            last_seen: Instant::now(),
        },
    );

    Ok(Json(PlayRegisterResponse { token, agent_id }))
}

pub async fn queue(
    State(app): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    Json(req): Json<PlayQueueRequest>,
) -> Result<Json<PlayOk>, (StatusCode, Json<PlayError>)> {
    let token = token_of(&headers, q.token)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "missing token"))?;
    let best_of = game::sanitize_best_of(req.best_of);

    let player = {
        let mut sessions = app.http_sessions.lock().await;
        let s = sessions
            .get_mut(&token)
            .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "unknown token"))?;
        s.last_seen = Instant::now();
        if s.queued {
            return Err(err(StatusCode::BAD_REQUEST, "already queued"));
        }
        let in_rx = s
            .in_rx
            .take()
            .ok_or_else(|| err(StatusCode::BAD_REQUEST, "session not joinable"))?;
        let out_tx = s.out_tx.take().expect("out_tx present with in_rx");
        s.queued = true;
        PlayerConn {
            agent_id: s.agent_id,
            player_id: s.player_id,
            model: s.model.clone(),
            display_name: s.display_name.clone(),
            test: s.test,
            out: out_tx,
            inbox: in_rx,
        }
    };

    app.matchmaker.enqueue(best_of, player).await;
    Ok(Json(PlayOk { ok: true }))
}

/// Push a `ClientMsg` into a session's engine channel (shared by commit/reveal/chat).
async fn forward(
    app: &Arc<AppState>,
    token: Uuid,
    msg: ClientMsg,
) -> Result<(), (StatusCode, Json<PlayError>)> {
    let sessions = app.http_sessions.lock().await;
    let s = sessions
        .get(&token)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "unknown token"))?;
    s.in_tx
        .send(msg)
        .map_err(|_| err(StatusCode::BAD_REQUEST, "match not active"))
}

pub async fn commit(
    State(app): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    Json(req): Json<PlayCommitRequest>,
) -> Result<Json<PlayOk>, (StatusCode, Json<PlayError>)> {
    let token = token_of(&headers, q.token)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "missing token"))?;
    {
        let mut sessions = app.http_sessions.lock().await;
        let s = sessions
            .get_mut(&token)
            .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "unknown token"))?;
        s.last_seen = Instant::now();
        // Idempotent: a retried commit for the same attempt is a no-op.
        if !s.committed.insert(req.attempt_id) {
            return Ok(Json(PlayOk { ok: true }));
        }
    }
    forward(
        &app,
        token,
        ClientMsg::Commit {
            attempt_id: req.attempt_id,
            hash: req.hash,
        },
    )
    .await?;
    Ok(Json(PlayOk { ok: true }))
}

pub async fn reveal(
    State(app): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    Json(req): Json<PlayRevealRequest>,
) -> Result<Json<PlayOk>, (StatusCode, Json<PlayError>)> {
    let token = token_of(&headers, q.token)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "missing token"))?;
    {
        let mut sessions = app.http_sessions.lock().await;
        let s = sessions
            .get_mut(&token)
            .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "unknown token"))?;
        s.last_seen = Instant::now();
        if !s.revealed.insert(req.attempt_id) {
            return Ok(Json(PlayOk { ok: true }));
        }
    }
    forward(
        &app,
        token,
        ClientMsg::Reveal {
            attempt_id: req.attempt_id,
            secret: req.secret,
        },
    )
    .await?;
    Ok(Json(PlayOk { ok: true }))
}

pub async fn chat(
    State(app): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    Json(req): Json<PlayChatRequest>,
) -> Result<Json<PlayOk>, (StatusCode, Json<PlayError>)> {
    let token = token_of(&headers, q.token)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "missing token"))?;
    let text = req.text.trim();
    if text.is_empty() || text.len() > MAX_CHAT_LEN {
        return Err(err(StatusCode::BAD_REQUEST, "text must be 1..=2000 chars"));
    }
    {
        let mut sessions = app.http_sessions.lock().await;
        if let Some(s) = sessions.get_mut(&token) {
            s.last_seen = Instant::now();
        }
    }
    forward(
        &app,
        token,
        ClientMsg::Chat {
            match_id: Uuid::nil(), // engine records using the live match id
            text: text.to_string(),
        },
    )
    .await?;
    Ok(Json(PlayOk { ok: true }))
}

pub async fn poll(
    State(app): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<PollQuery>,
) -> Result<Json<PlayPollResponse>, (StatusCode, Json<PlayError>)> {
    let token = token_of(&headers, q.token)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "missing token"))?;
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let timeout = Duration::from_millis(q.timeout_ms.unwrap_or(0).min(30_000));

    let (outbox, notify) = {
        let mut sessions = app.http_sessions.lock().await;
        let s = sessions
            .get_mut(&token)
            .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "unknown token"))?;
        s.last_seen = Instant::now();
        (s.outbox.clone(), s.notify.clone())
    };

    let mut msgs = drain(&outbox, limit).await;
    if msgs.is_empty() && !timeout.is_zero() {
        let _ = tokio::time::timeout(timeout, notify.notified()).await;
        msgs = drain(&outbox, limit).await;
    }

    // Once the match ends, tidy up the session.
    if msgs.iter().any(|m| matches!(m, ServerMsg::MatchEnd { .. })) {
        app.http_sessions.lock().await.remove(&token);
    }

    Ok(Json(PlayPollResponse { messages: msgs }))
}
