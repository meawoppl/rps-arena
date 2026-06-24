use serde::{Deserialize, Serialize};
use uuid::Uuid;
use ws_bridge::WsEndpoint;

// ---------------------------------------------------------------------------
// WebSocket endpoint definition — single source of truth for server + client
// ---------------------------------------------------------------------------

/// The main application WebSocket endpoint.
pub struct AppSocket;

impl WsEndpoint for AppSocket {
    const PATH: &'static str = "/ws";
    type ServerMsg = ServerMsg;
    type ClientMsg = ClientMsg;
}

/// Messages sent from the server to the client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerMsg {
    /// Heartbeat to keep connection alive
    Heartbeat,

    /// Error from server
    Error { message: String },

    /// Server is shutting down
    ServerShutdown {
        reason: String,
        reconnect_delay_ms: u64,
    },
}

/// Messages sent from the client to the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMsg {
    /// Ping — server should respond with Heartbeat
    Ping,
}

// ---------------------------------------------------------------------------
// HTTP API types
// ---------------------------------------------------------------------------

/// Health check response from `/api/health`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
}

/// Example API item (matches the `items` database table).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: Uuid,
    pub name: String,
    pub created_at: chrono::NaiveDateTime,
}

/// Request body for creating a new item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateItemRequest {
    pub name: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_msg_heartbeat_roundtrip() {
        let msg = ServerMsg::Heartbeat;
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMsg = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, ServerMsg::Heartbeat));
    }

    #[test]
    fn server_msg_error_roundtrip() {
        let msg = ServerMsg::Error {
            message: "something broke".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMsg = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMsg::Error { message } => assert_eq!(message, "something broke"),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn server_msg_shutdown_roundtrip() {
        let msg = ServerMsg::ServerShutdown {
            reason: "restarting".to_string(),
            reconnect_delay_ms: 1000,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMsg = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMsg::ServerShutdown {
                reason,
                reconnect_delay_ms,
            } => {
                assert_eq!(reason, "restarting");
                assert_eq!(reconnect_delay_ms, 1000);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn client_msg_ping_roundtrip() {
        let msg = ClientMsg::Ping;
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMsg = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, ClientMsg::Ping));
    }

    #[test]
    fn item_roundtrip() {
        let item = Item {
            id: Uuid::new_v4(),
            name: "test item".to_string(),
            created_at: chrono::Utc::now().naive_utc(),
        };
        let json = serde_json::to_string(&item).unwrap();
        let parsed: Item = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, item.id);
        assert_eq!(parsed.name, item.name);
    }
}
