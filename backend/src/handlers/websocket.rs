use std::sync::Arc;

use axum::{
    extract::ws::WebSocketUpgrade,
    response::Response,
    routing::{self, MethodRouter},
};
use tokio::sync::mpsc;
use uuid::Uuid;

use shared::{AgentSocket, AllowedModelNames, ClientMsg, ServerMsg};

use crate::client_ip::ClientIp;
use crate::game::{self, PlayerConn, QueueHeartbeat};
use crate::{db_ops, AppState};

const ENGINE_INBOX_CAP: usize = 128;
const ENGINE_OUTBOX_CAP: usize = 128;

/// WebSocket route handler for [`AgentSocket`].
///
/// Each connection runs an IO multiplexer (this task) that bridges the typed
/// socket to a pair of channels, plus a [`session`] task that handles the
/// register/queue handshake and then hands the player to the [`game::Matchmaker`].
pub fn handler(state: Arc<AppState>) -> MethodRouter {
    routing::get(
        move |ws: WebSocketUpgrade, ClientIp(ip): ClientIp| async move {
            handle_upgrade(ws, state.clone(), ip)
        },
    )
}

fn handle_upgrade(ws: WebSocketUpgrade, state: Arc<AppState>, ip: std::net::IpAddr) -> Response {
    ws_bridge::server::upgrade::<AgentSocket, _, _>(ws, move |mut conn| {
        let state = state.clone();
        async move {
            let (in_tx, in_rx) = mpsc::channel::<ClientMsg>(ENGINE_INBOX_CAP);
            let (out_tx, mut out_rx) = mpsc::channel::<ServerMsg>(ENGINE_OUTBOX_CAP);
            let queue_heartbeat = QueueHeartbeat::new();

            tokio::spawn(session(
                state,
                Some(ip),
                in_rx,
                out_tx,
                queue_heartbeat.clone(),
            ));

            // Bridge: socket <-> channels. Ends when either side closes.
            loop {
                tokio::select! {
                    out = out_rx.recv() => match out {
                        Some(m) => {
                            if conn.send(m).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    },
                    inc = conn.recv() => match inc {
                        Some(Ok(m)) => {
                            queue_heartbeat.touch();
                            if in_tx.try_send(m).is_err() {
                                let _ = conn
                                    .send(ServerMsg::Error {
                                        message: "websocket input queue full".into(),
                                    })
                                    .await;
                                break;
                            }
                        }
                        Some(Err(e)) => {
                            let _ = conn
                                .send(ServerMsg::Error {
                                    message: format!("decode error: {e}"),
                                })
                                .await;
                        }
                        None => break,
                    },
                }
            }
        }
    })
}

/// Handshake: wait for `Register`, then `JoinQueue`, then enqueue for a match.
async fn session(
    state: Arc<AppState>,
    ip: Option<std::net::IpAddr>,
    mut in_rx: mpsc::Receiver<ClientMsg>,
    out_tx: mpsc::Sender<ServerMsg>,
    queue_heartbeat: QueueHeartbeat,
) {
    let (model, display_name) = loop {
        match in_rx.recv().await {
            Some(ClientMsg::Register {
                model,
                display_name,
            }) => {
                let model = match AllowedModelNames::normalize(&model) {
                    Ok(model) => model,
                    Err(err) => {
                        let _ = out_tx.try_send(ServerMsg::Error {
                            message: format!(
                                "{}; {}",
                                err.message(),
                                AllowedModelNames::describe()
                            ),
                        });
                        continue;
                    }
                };
                let display_name = match AllowedModelNames::display_name_for(&model, &display_name)
                {
                    Ok(display_name) => display_name,
                    Err(err) => {
                        let _ = out_tx.try_send(ServerMsg::Error {
                            message: err.message().into(),
                        });
                        continue;
                    }
                };
                break (model, display_name);
            }
            Some(ClientMsg::Ping) => {
                let _ = out_tx.try_send(ServerMsg::Heartbeat);
            }
            Some(_) => {
                let _ = out_tx.try_send(ServerMsg::Error {
                    message: "send Register first".into(),
                });
            }
            None => return,
        }
    };

    let agent_id = Uuid::new_v4();
    let player_id = Uuid::new_v4();
    if let Err(e) = db_ops::create_player(
        &state.db_pool,
        player_id,
        model.clone(),
        display_name.clone(),
    )
    .await
    {
        tracing::warn!("create_player failed: {e}");
    }
    let _ = out_tx.try_send(ServerMsg::Registered { agent_id });

    let best_of = loop {
        match in_rx.recv().await {
            Some(ClientMsg::JoinQueue { best_of }) => break game::sanitize_best_of(best_of),
            Some(ClientMsg::Ping) => {
                let _ = out_tx.try_send(ServerMsg::Heartbeat);
            }
            Some(_) => {
                let _ = out_tx.try_send(ServerMsg::Error {
                    message: "send JoinQueue to enter matchmaking".into(),
                });
            }
            None => return,
        }
    };

    let player = PlayerConn {
        agent_id,
        player_id,
        model,
        display_name,
        out: out_tx,
        inbox: in_rx,
        queue_heartbeat,
    };
    state.matchmaker.enqueue(best_of, ip, player).await;
}
