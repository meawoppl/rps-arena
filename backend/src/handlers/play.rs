//! Curl-friendly REST polling API over the match engine.
//!
//! An HTTP "player" is just a different transport feeding the same mpsc
//! channels the WebSocket path uses — `run_match`/matchmaker are unchanged.
//! Flow: `register` → `queue` → poll `poll` and react (on `RoundStart` POST
//! `commit`, on `AwaitReveal` POST `reveal`), optionally `chat`.
//!
//! Auth: the `register` token is passed as `Authorization: Bearer <token>`.
//! The token is a secret — it controls the player — and is never accepted via
//! query string.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
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
    validate_chat, validate_commit_hash, validate_reveal_secret, validate_strategy_summary,
    AllowedModelNames, ClientMsg, PlayChatRequest, PlayCommitRequest, PlayError, PlayOk,
    PlayPollResponse, PlayQueueRequest, PlayRegisterRequest, PlayRegisterResponse,
    PlayRevealRequest, ServerMsg,
};

use crate::client_ip::ClientIp;
use crate::game::{self, PlayerConn};
use crate::{db_ops, AppState};

/// Max buffered server messages per HTTP session before we cut it loose.
const OUTBOX_CAP: usize = 512;
/// Max client->engine messages buffered per HTTP session.
const ENGINE_INBOX_CAP: usize = 128;
/// Max engine->HTTP bridge messages buffered before the outbox task drains them.
const ENGINE_OUTBOX_CAP: usize = 128;
/// Idle session TTL (no requests for this long → reaped).
const IDLE_TTL: Duration = Duration::from_secs(600);
/// Minimum interval between accepted commits — at most one turn per second
/// (#29), enforced before the commit is forwarded to the engine so the HTTP
/// client can simply retry after the cooldown.
const MIN_TURN_INTERVAL: Duration = Duration::from_secs(1);
#[derive(Debug, Clone, PartialEq, Eq)]
struct CommitReceipt {
    hash: String,
    strategy_summary: String,
}

/// Server-side state for one HTTP player.
pub struct HttpSession {
    agent_id: Uuid,
    player_id: Uuid,
    model: String,
    display_name: String,
    /// Sender into the engine (filled by POST commit/reveal/chat).
    in_tx: mpsc::Sender<ClientMsg>,
    /// Receiver + outbound sender, taken when the player joins the queue.
    in_rx: Option<mpsc::Receiver<ClientMsg>>,
    out_tx: Option<mpsc::Sender<ServerMsg>>,
    /// Buffered server→player messages, drained by `poll`.
    outbox: Arc<Mutex<VecDeque<ServerMsg>>>,
    notify: Arc<Notify>,
    committed: HashMap<Uuid, CommitReceipt>,
    revealed: HashMap<Uuid, String>,
    queued: bool,
    last_seen: Instant,
    /// Timestamp of the last accepted (forwarded) commit — enforces #29's
    /// "at most one turn per second" before the commit reaches the engine.
    last_commit_at: Option<Instant>,
    /// Client IP captured at register, used by the matchmaker for the
    /// self-pairing guard + per-IP concurrency cap (#32).
    ip: Option<IpAddr>,
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
pub struct PollQuery {
    timeout_ms: Option<u64>,
    limit: Option<usize>,
}

fn err(code: StatusCode, msg: &str) -> (StatusCode, Json<PlayError>) {
    (code, Json(PlayError { error: msg.into() }))
}

/// Token from `Authorization: Bearer <token>` only. The token is a secret, so
/// it is never accepted via query string (`?token=` leaks into access logs,
/// browser history, and `Referer` headers).
fn token_of(headers: &HeaderMap) -> Option<Uuid> {
    let val = headers.get(AUTHORIZATION).and_then(|h| h.to_str().ok())?;
    let t = val
        .strip_prefix("Bearer ")
        .or_else(|| val.strip_prefix("bearer "))?;
    Uuid::parse_str(t.trim()).ok()
}

async fn drain(outbox: &Arc<Mutex<VecDeque<ServerMsg>>>, limit: usize) -> Vec<ServerMsg> {
    let mut ob = outbox.lock().await;
    let n = ob.len().min(limit);
    ob.drain(..n).collect()
}

/// Outcome of an idempotency check for a locked, attempt-scoped value
/// (a commit hash or a reveal secret).
#[derive(Debug, PartialEq, Eq)]
enum Idempotency {
    /// No prior value for this attempt — accept and forward it.
    Fresh,
    /// Exact retry of the same value — treat as success, forward nothing.
    Duplicate,
    /// A different value for an already-locked attempt — reject.
    Conflict,
}

/// Decide how to treat an incoming attempt-scoped value given any prior one.
///
/// Shared by `commit` (hash) and `reveal` (secret): replaying the same value is
/// an idempotent retry (safe for a flaky human client to resend), while a
/// *different* value for the same attempt means the client is trying to change
/// a locked commitment and must be refused.
fn idempotency_of(prior: Option<&String>, incoming: &str) -> Idempotency {
    match prior {
        Some(existing) if existing == incoming => Idempotency::Duplicate,
        Some(_) => Idempotency::Conflict,
        None => Idempotency::Fresh,
    }
}

pub async fn register(
    State(app): State<Arc<AppState>>,
    ClientIp(client_ip): ClientIp,
    Json(req): Json<PlayRegisterRequest>,
) -> Result<Json<PlayRegisterResponse>, (StatusCode, Json<PlayError>)> {
    let model = AllowedModelNames::normalize(&req.model).map_err(|model_err| {
        err(
            StatusCode::BAD_REQUEST,
            &format!("{}; {}", model_err.message(), AllowedModelNames::describe()),
        )
    })?;
    let display_name = AllowedModelNames::display_name_for(&model, &req.display_name)
        .map_err(|display_err| err(StatusCode::BAD_REQUEST, display_err.message()))?;
    let agent_id = Uuid::new_v4();
    let player_id = Uuid::new_v4();
    if let Err(e) =
        db_ops::create_player(&app.db_pool, player_id, model.clone(), display_name.clone()).await
    {
        tracing::warn!("create_player failed: {e}");
    }

    let (in_tx, in_rx) = mpsc::channel::<ClientMsg>(ENGINE_INBOX_CAP);
    let (out_tx, mut out_rx) = mpsc::channel::<ServerMsg>(ENGINE_OUTBOX_CAP);
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
            model,
            display_name,
            in_tx,
            in_rx: Some(in_rx),
            out_tx: Some(out_tx),
            outbox,
            notify,
            committed: HashMap::new(),
            revealed: HashMap::new(),
            queued: false,
            last_seen: Instant::now(),
            last_commit_at: None,
            ip: Some(client_ip),
        },
    );

    Ok(Json(PlayRegisterResponse { token, agent_id }))
}

pub async fn queue(
    State(app): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<PlayQueueRequest>,
) -> Result<Json<PlayOk>, (StatusCode, Json<PlayError>)> {
    let token = token_of(&headers).ok_or_else(|| err(StatusCode::UNAUTHORIZED, "missing token"))?;
    let best_of = game::sanitize_best_of(req.best_of);

    let (ip, player) = {
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
        (
            s.ip,
            PlayerConn {
                agent_id: s.agent_id,
                player_id: s.player_id,
                model: s.model.clone(),
                display_name: s.display_name.clone(),
                out: out_tx,
                inbox: in_rx,
            },
        )
    };

    app.matchmaker.enqueue(best_of, ip, player).await;
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
    s.in_tx.try_send(msg).map_err(|send_err| {
        if matches!(send_err, mpsc::error::TrySendError::Full(_)) {
            err(StatusCode::TOO_MANY_REQUESTS, "match input queue full")
        } else {
            err(StatusCode::BAD_REQUEST, "match not active")
        }
    })
}

pub async fn commit(
    State(app): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<PlayCommitRequest>,
) -> Result<Json<PlayOk>, (StatusCode, Json<PlayError>)> {
    let token = token_of(&headers).ok_or_else(|| err(StatusCode::UNAUTHORIZED, "missing token"))?;
    let strategy_summary = validate_strategy_summary(&req.strategy_summary)
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, "strategy_summary required"))?
        .to_string();
    if !validate_commit_hash(&req.hash) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "hash must be 64 lowercase hex chars",
        ));
    }
    let incoming = CommitReceipt {
        hash: req.hash.clone(),
        strategy_summary: strategy_summary.clone(),
    };
    {
        let mut sessions = app.http_sessions.lock().await;
        let s = sessions
            .get_mut(&token)
            .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "unknown token"))?;
        s.last_seen = Instant::now();
        // Idempotent only for an exact retry. A different hash or summary for
        // the same attempt means the client is trying to change locked commit
        // metadata.
        let idempotency = match s.committed.get(&req.attempt_id) {
            Some(existing) if existing == &incoming => Idempotency::Duplicate,
            Some(_) => Idempotency::Conflict,
            None => Idempotency::Fresh,
        };
        match idempotency {
            Idempotency::Duplicate => return Ok(Json(PlayOk { ok: true })),
            Idempotency::Conflict => {
                return Err(err(
                    StatusCode::CONFLICT,
                    "conflicting commit for attempt_id",
                ))
            }
            Idempotency::Fresh => {}
        }
        // #29: a fresh commit no sooner than 1s after the previous one.
        // Retryable (429) — the client just waits out the cooldown and resends.
        if s.last_commit_at
            .is_some_and(|t| t.elapsed() < MIN_TURN_INTERVAL)
        {
            return Err(err(
                StatusCode::TOO_MANY_REQUESTS,
                "slow down — at most one turn per second",
            ));
        }
        s.in_tx
            .try_send(ClientMsg::Commit {
                attempt_id: req.attempt_id,
                hash: req.hash.clone(),
                strategy_summary,
            })
            .map_err(|send_err| {
                if matches!(send_err, mpsc::error::TrySendError::Full(_)) {
                    err(StatusCode::TOO_MANY_REQUESTS, "match input queue full")
                } else {
                    err(StatusCode::BAD_REQUEST, "match not active")
                }
            })?;
        s.last_commit_at = Some(Instant::now());
        s.committed.insert(req.attempt_id, incoming);
    }
    Ok(Json(PlayOk { ok: true }))
}

pub async fn reveal(
    State(app): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<PlayRevealRequest>,
) -> Result<Json<PlayOk>, (StatusCode, Json<PlayError>)> {
    let token = token_of(&headers).ok_or_else(|| err(StatusCode::UNAUTHORIZED, "missing token"))?;
    if !validate_reveal_secret(&req.secret) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "secret must be '<throw>:<hex nonce>' and at most 256 bytes",
        ));
    }
    {
        let mut sessions = app.http_sessions.lock().await;
        let s = sessions
            .get_mut(&token)
            .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "unknown token"))?;
        s.last_seen = Instant::now();
        match idempotency_of(s.revealed.get(&req.attempt_id), &req.secret) {
            Idempotency::Duplicate => return Ok(Json(PlayOk { ok: true })),
            Idempotency::Conflict => {
                return Err(err(
                    StatusCode::CONFLICT,
                    "conflicting reveal for attempt_id",
                ))
            }
            Idempotency::Fresh => {}
        }
        s.in_tx
            .try_send(ClientMsg::Reveal {
                attempt_id: req.attempt_id,
                secret: req.secret.clone(),
            })
            .map_err(|send_err| {
                if matches!(send_err, mpsc::error::TrySendError::Full(_)) {
                    err(StatusCode::TOO_MANY_REQUESTS, "match input queue full")
                } else {
                    err(StatusCode::BAD_REQUEST, "match not active")
                }
            })?;
        s.revealed.insert(req.attempt_id, req.secret);
    }
    Ok(Json(PlayOk { ok: true }))
}

pub async fn chat(
    State(app): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<PlayChatRequest>,
) -> Result<Json<PlayOk>, (StatusCode, Json<PlayError>)> {
    let token = token_of(&headers).ok_or_else(|| err(StatusCode::UNAUTHORIZED, "missing token"))?;
    let text = validate_chat(&req.text)
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, "text must be 1..=2000 chars"))?;
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
    let token = token_of(&headers).ok_or_else(|| err(StatusCode::UNAUTHORIZED, "missing token"))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use shared::MAX_CHAT_LEN;

    fn bearer(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, HeaderValue::from_str(value).unwrap());
        h
    }

    #[test]
    fn token_parsed_from_bearer_header_case_insensitive_and_trimmed() {
        let tok = Uuid::new_v4();
        assert_eq!(token_of(&bearer(&format!("Bearer {tok}"))), Some(tok));
        assert_eq!(token_of(&bearer(&format!("bearer {tok}"))), Some(tok));
        assert_eq!(token_of(&bearer(&format!("Bearer  {tok}  "))), Some(tok));
    }

    #[test]
    fn token_is_none_without_a_valid_bearer_header() {
        // No header, wrong scheme, or unparsable token — and never from a query
        // string (which is no longer read).
        assert_eq!(token_of(&HeaderMap::new()), None);
        assert_eq!(token_of(&bearer("Bearer not-a-uuid")), None);
        assert_eq!(token_of(&bearer("Basic Zm9vOmJhcg==")), None);
        assert_eq!(token_of(&bearer("Bearer garbage")), None);
    }

    #[tokio::test]
    async fn drain_respects_limit_and_preserves_fifo_order() {
        let outbox = Arc::new(Mutex::new(VecDeque::new()));
        {
            let mut ob = outbox.lock().await;
            for position in 0..5 {
                ob.push_back(ServerMsg::Queued {
                    best_of: 3,
                    position,
                });
            }
        }
        // First drain takes only `limit` from the front, in order.
        let first = drain(&outbox, 2).await;
        assert_eq!(first.len(), 2);
        assert!(matches!(first[0], ServerMsg::Queued { position: 0, .. }));
        assert!(matches!(first[1], ServerMsg::Queued { position: 1, .. }));

        // A generous limit drains whatever remains without error.
        let rest = drain(&outbox, 100).await;
        assert_eq!(rest.len(), 3);
        assert!(matches!(rest[2], ServerMsg::Queued { position: 4, .. }));

        // Draining an empty outbox yields nothing.
        assert!(drain(&outbox, 50).await.is_empty());
    }

    #[test]
    fn idempotency_fresh_when_no_prior_value() {
        assert_eq!(idempotency_of(None, "abc"), Idempotency::Fresh);
    }

    #[test]
    fn idempotency_duplicate_on_exact_retry() {
        let prior = "deadbeef".to_string();
        assert_eq!(
            idempotency_of(Some(&prior), "deadbeef"),
            Idempotency::Duplicate
        );
    }

    #[test]
    fn idempotency_conflict_on_changed_value() {
        let prior = "deadbeef".to_string();
        assert_eq!(
            idempotency_of(Some(&prior), "feedface"),
            Idempotency::Conflict
        );
    }

    #[test]
    fn validate_chat_trims_and_accepts_normal_text() {
        assert_eq!(validate_chat("  hello  "), Some("hello"));
        assert_eq!(validate_chat("gg"), Some("gg"));
    }

    #[test]
    fn validate_chat_rejects_empty_and_whitespace_only() {
        assert_eq!(validate_chat(""), None);
        assert_eq!(validate_chat("   \t\n "), None);
    }

    #[test]
    fn validate_chat_enforces_max_len_on_trimmed_text() {
        let at_cap = "x".repeat(MAX_CHAT_LEN);
        assert_eq!(validate_chat(&at_cap).map(|s| s.len()), Some(MAX_CHAT_LEN));

        let over_cap = "y".repeat(MAX_CHAT_LEN + 1);
        assert_eq!(validate_chat(&over_cap), None);

        // Surrounding whitespace is trimmed before the length check, so an
        // over-cap body padded down to the cap still passes.
        let padded = format!("  {}  ", "z".repeat(MAX_CHAT_LEN));
        assert_eq!(validate_chat(&padded).map(|s| s.len()), Some(MAX_CHAT_LEN));
    }
}
