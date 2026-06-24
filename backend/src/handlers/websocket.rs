use axum::routing::MethodRouter;
use shared::{AgentSocket, ClientMsg, ServerMsg};

/// Returns the WebSocket route handler for [`AgentSocket`].
///
/// This is a protocol-layer stub: it keeps the connection alive and rejects
/// gameplay messages until the match engine lands (M2).
///
/// Wire this into the router with:
/// ```ignore
/// .route(AgentSocket::PATH, handlers::websocket::handler())
/// ```
pub fn handler() -> MethodRouter {
    ws_bridge::server::handler::<AgentSocket, _, _>(|mut conn| async move {
        let _ = conn.send(ServerMsg::Heartbeat).await;

        while let Some(result) = conn.recv().await {
            match result {
                Ok(ClientMsg::Ping) => {
                    let _ = conn.send(ServerMsg::Heartbeat).await;
                }
                Ok(_) => {
                    let _ = conn
                        .send(ServerMsg::Error {
                            message: "match engine not yet implemented (M2)".to_string(),
                        })
                        .await;
                }
                Err(e) => {
                    let _ = conn
                        .send(ServerMsg::Error {
                            message: format!("Decode error: {e}"),
                        })
                        .await;
                }
            }
        }
    })
}
