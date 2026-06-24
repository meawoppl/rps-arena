use axum::routing::MethodRouter;
use shared::{AppSocket, ClientMsg, ServerMsg};

/// Returns the WebSocket route handler for [`AppSocket`].
///
/// Wire this into the router with:
/// ```ignore
/// .route(AppSocket::PATH, handlers::websocket::handler())
/// ```
pub fn handler() -> MethodRouter {
    ws_bridge::server::handler::<AppSocket, _, _>(|mut conn| async move {
        // Send initial heartbeat
        let _ = conn.send(ServerMsg::Heartbeat).await;

        // Receive loop — messages arrive fully typed
        while let Some(result) = conn.recv().await {
            match result {
                Ok(ClientMsg::Ping) => {
                    let _ = conn.send(ServerMsg::Heartbeat).await;
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
