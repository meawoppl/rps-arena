use std::sync::Arc;

use axum::routing::MethodRouter;
use tokio::sync::mpsc;
use uuid::Uuid;

use shared::{AgentSocket, AllowedModelNames, ClientMsg, ServerMsg};

use crate::game::{self, PlayerConn};
use crate::{db_ops, AppState};

/// WebSocket route handler for [`AgentSocket`].
///
/// Each connection runs an IO multiplexer (this task) that bridges the typed
/// socket to a pair of channels, plus a [`session`] task that handles the
/// register/queue handshake and then hands the player to the [`game::Matchmaker`].
pub fn handler(state: Arc<AppState>) -> MethodRouter {
    ws_bridge::server::handler::<AgentSocket, _, _>(move |mut conn| {
        let state = state.clone();
        async move {
            let (in_tx, in_rx) = mpsc::unbounded_channel::<ClientMsg>();
            let (out_tx, mut out_rx) = mpsc::unbounded_channel::<ServerMsg>();

            tokio::spawn(session(state, in_rx, out_tx));

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
                            if in_tx.send(m).is_err() {
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
    mut in_rx: mpsc::UnboundedReceiver<ClientMsg>,
    out_tx: mpsc::UnboundedSender<ServerMsg>,
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
                        let _ = out_tx.send(ServerMsg::Error {
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
                        let _ = out_tx.send(ServerMsg::Error {
                            message: err.message().into(),
                        });
                        continue;
                    }
                };
                break (model, display_name);
            }
            Some(ClientMsg::Ping) => {
                let _ = out_tx.send(ServerMsg::Heartbeat);
            }
            Some(_) => {
                let _ = out_tx.send(ServerMsg::Error {
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
    let _ = out_tx.send(ServerMsg::Registered { agent_id });

    let best_of = loop {
        match in_rx.recv().await {
            Some(ClientMsg::JoinQueue { best_of }) => break game::sanitize_best_of(best_of),
            Some(ClientMsg::Ping) => {
                let _ = out_tx.send(ServerMsg::Heartbeat);
            }
            Some(_) => {
                let _ = out_tx.send(ServerMsg::Error {
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
    };
    state.matchmaker.enqueue(best_of, player).await;
}
