# Rust Web App Skeleton

Template for full-stack Rust web applications with a Yew WASM frontend embedded into an Axum backend as a single binary.

Patterns extracted from [agent-portal](https://github.com/meawoppl/agent-portal) and [inboxnegative.com](https://github.com/meawoppl/inboxnegative.com).

## Architecture

```
Cargo.toml                        # Workspace root (backend, frontend, shared)
├── shared/src/lib.rs             # Serde types used by both sides
├── frontend/
│   ├── Trunk.toml                # WASM bundler config
│   ├── index.html                # Trunk entry point
│   └── src/main.rs               # Yew App with routing
├── backend/
│   ├── src/
│   │   ├── main.rs               # Axum server, routes, shutdown
│   │   ├── db.rs                 # Diesel pool + embedded migrations
│   │   ├── models.rs             # Queryable/Insertable structs
│   │   ├── schema.rs             # Diesel generated schema
│   │   ├── embedded_assets.rs    # rust-embed SPA serving
│   │   └── handlers/
│   │       ├── health.rs         # GET /api/health
│   │       └── websocket.rs      # ws-bridge typed WebSocket
│   ├── diesel.toml
│   └── migrations/
├── Dockerfile                    # Single binary deploy
├── docker-compose.yml            # Postgres + backend
├── scripts/check-migration-names.sh
└── .github/workflows/
    ├── ci.yml                    # lint, audit, fmt, clippy, build, test
    └── container.yml             # Docker image -> GHCR
```

## Quick Start

```sh
# Prerequisites
rustup target add wasm32-unknown-unknown
cargo install trunk --locked

# Start Postgres
docker compose up db -d

# Copy env
cp .env.example .env

# Build frontend (must happen before backend due to rust-embed)
cd frontend && trunk build && cd ..

# Run
cargo run -p backend -- --dev-mode
# -> http://localhost:3000
```

---

## Pattern 1: Shared Types + ws-bridge Endpoint (`shared/src/lib.rs`)

Both the backend and frontend depend on the `shared` crate. All protocol messages and API types live here as proper structs -- no `json!` macro.

The WebSocket protocol is defined using [ws-bridge](https://crates.io/crates/ws-bridge), which provides a `WsEndpoint` trait for strongly-typed WebSocket connections. A single struct defines the path and message types -- both sides reference this as the single source of truth:

```rust
use ws_bridge::WsEndpoint;

pub struct AppSocket;

impl WsEndpoint for AppSocket {
    const PATH: &'static str = "/ws";
    type ServerMsg = ServerMsg;
    type ClientMsg = ClientMsg;
}
```

Server and client messages are **separate enums** using serde-tagged serialization (`{"type": "Variant", ...}`):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerMsg {
    Heartbeat,
    Error { message: String },
    ServerShutdown { reason: String, reconnect_delay_ms: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMsg {
    Ping,
}
```

API responses are also typed structs:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: Uuid,
    pub name: String,
    pub created_at: chrono::NaiveDateTime,
}
```

Every type gets a **roundtrip serialization test**:

```rust
#[test]
fn server_msg_error_roundtrip() {
    let msg = ServerMsg::Error { message: "something broke".to_string() };
    let json = serde_json::to_string(&msg).unwrap();
    let parsed: ServerMsg = serde_json::from_str(&json).unwrap();
    match parsed {
        ServerMsg::Error { message } => assert_eq!(message, "something broke"),
        _ => panic!("Wrong variant"),
    }
}
```

To add a new message type: add a variant to `ServerMsg` or `ClientMsg`, add a roundtrip test, and both sides can immediately use it.

---

## Pattern 2: Frontend Embedding (`backend/src/embedded_assets.rs`)

Trunk compiles the Yew frontend to `frontend/dist/`. The backend uses `rust-embed` to bake those files into the binary at compile time:

```rust
#[derive(RustEmbed)]
#[folder = "../frontend/dist"]
pub struct FrontendAssets;
```

The serve function handles both static assets and SPA fallback (unknown paths return `index.html` so client-side routing works):

```rust
pub async fn serve_embedded_frontend(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    match FrontendAssets::get(path) {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            (StatusCode::OK, [(header::CONTENT_TYPE, mime.as_ref())],
             Body::from(content.data.to_vec())).into_response()
        }
        None => {
            // SPA fallback: any unknown path -> index.html
            match FrontendAssets::get("index.html") {
                Some(content) => (StatusCode::OK, [(header::CONTENT_TYPE, "text/html")],
                     Body::from(content.data.to_vec())).into_response(),
                None => (StatusCode::NOT_FOUND, "Frontend not found").into_response(),
            }
        }
    }
}
```

Wired into the router as a fallback so `/api/*` routes take priority:

```rust
let app = Router::new()
    .route("/api/health", get(handlers::health::health))
    .route("/ws", get(handlers::websocket::ws_handler))
    .with_state(app_state)
    .fallback(axum::routing::get(embedded_assets::serve_embedded_frontend))
    .layer(cors);
```

The result is a **single binary** with no external file dependencies. `frontend/dist/` is not needed at runtime.

---

## Pattern 3: Typed WebSockets with ws-bridge (`backend/src/handlers/websocket.rs`)

Uses [ws-bridge](https://crates.io/crates/ws-bridge) for strongly-typed WebSocket connections. The handler references the `AppSocket` endpoint defined in `shared/`, so message types are enforced at compile time -- no manual serde or raw strings:

```rust
use shared::{AppSocket, ClientMsg, ServerMsg};

pub fn handler() -> MethodRouter {
    ws_bridge::server::handler::<AppSocket, _, _>(|mut conn| async move {
        // Send initial heartbeat
        let _ = conn.send(ServerMsg::Heartbeat).await;

        // Receive loop — all messages are already deserialized
        while let Some(result) = conn.recv().await {
            match result {
                Ok(ClientMsg::Ping) => {
                    let _ = conn.send(ServerMsg::Heartbeat).await;
                }
                Err(e) => {
                    let _ = conn.send(ServerMsg::Error {
                        message: format!("Decode error: {e}"),
                    }).await;
                }
            }
        }
    })
}
```

The route uses the path from the endpoint definition:

```rust
.route(shared::AppSocket::PATH, handlers::websocket::handler())
```

ws-bridge handles all the Axum WebSocket upgrade plumbing, JSON serialization, and type enforcement. To add new message types, just add variants to `ServerMsg`/`ClientMsg` in `shared/` -- both backend and frontend immediately see them.

---

## Pattern 4: Diesel Database (`backend/src/db.rs` + `models.rs`)

Migrations are **embedded into the binary** and run automatically on startup:

```rust
// db.rs
pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

pub fn create_pool() -> Result<DbPool> {
    let database_url = env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let manager = ConnectionManager::<PgConnection>::new(database_url);
    let pool = r2d2::Pool::builder().build(manager).expect("Failed to create pool");
    Ok(pool)
}

pub fn run_migrations(pool: &DbPool) -> Result<Vec<String>> {
    let mut conn = pool.get()?;
    let applied: Vec<String> = conn
        .run_pending_migrations(MIGRATIONS)
        .map_err(|e| anyhow::anyhow!("Failed to run migrations: {}", e))?
        .iter().map(|m| m.to_string()).collect();
    Ok(applied)
}
```

Models have **separate Diesel structs and shared API structs**, connected by `From` impls:

```rust
// models.rs -- Diesel types (backend only)
#[derive(Debug, Queryable, Selectable)]
#[diesel(table_name = items)]
pub struct Item {
    pub id: Uuid,
    pub name: String,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = items)]
pub struct NewItem {
    pub name: String,
}

// Convert Diesel model -> shared API type
impl From<Item> for shared::Item {
    fn from(item: Item) -> Self {
        shared::Item { id: item.id, name: item.name, created_at: item.created_at }
    }
}
```

```sql
-- migrations/00000000000000_initial/up.sql
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";

CREATE TABLE items (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    name TEXT NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT NOW()
);
```

Generate new migrations with `diesel migration generate add_something`.

Migration naming is enforced by CI via `scripts/check-migration-names.sh`:
- `00000000000000_description` (initial)
- `YYYY-MM-DD-HHMMSS_description` (timestamped, snake_case)

---

## Pattern 5: Frontend with ws-bridge Client (`frontend/src/main.rs`)

The Yew frontend uses `gloo-net` for HTTP and `ws_bridge::yew_client` for typed WebSocket connections. Both reference types from the `shared` crate directly.

**HTTP fetch** (same shared types as the backend):

```rust
use gloo_net::http::Request;
use shared::HealthResponse;

spawn_local(async move {
    match Request::get("/api/health").send().await {
        Ok(resp) => {
            if let Ok(data) = resp.json::<HealthResponse>().await {
                health.set(Some(data.status));
            }
        }
        Err(e) => health.set(Some(format!("Error: {}", e))),
    }
});
```

**WebSocket via ws-bridge** -- `connect::<AppSocket>()` automatically derives the `ws://`/`wss://` URL from the page's location. Split the connection so send and receive run in independent tasks:

```rust
use shared::{AppSocket, ClientMsg, ServerMsg};

match ws_bridge::yew_client::connect::<AppSocket>() {
    Ok(conn) => {
        let (mut tx, mut rx) = conn.split();

        // Ping loop
        spawn_local(async move {
            loop {
                sleep(Duration::from_secs(5)).await;
                if tx.send(ClientMsg::Ping).await.is_err() {
                    break;
                }
            }
        });

        // Receive loop — messages are already deserialized
        spawn_local(async move {
            while let Some(result) = rx.recv().await {
                match result {
                    Ok(ServerMsg::Heartbeat) => { /* update UI */ }
                    Ok(ServerMsg::Error { message }) => { /* show error */ }
                    Ok(ServerMsg::ServerShutdown { .. }) => break,
                    Err(e) => { /* handle decode error */ break; }
                }
            }
        });
    }
    Err(e) => { /* connection failed */ }
}
```

Routes use `yew-router` with a `BrowserRouter` (works because the backend's SPA fallback serves `index.html` for all unknown paths):

```rust
#[derive(Clone, Routable, PartialEq)]
enum Route {
    #[at("/")]
    Home,
    #[not_found]
    #[at("/404")]
    NotFound,
}
```

---

## Pattern 6: Axum Server Setup (`backend/src/main.rs`)

Server startup follows a consistent sequence: parse args, init tracing, load env, create DB pool, run migrations, build router, serve with graceful shutdown.

```rust
#[derive(Parser, Debug, Clone)]
struct Args {
    #[arg(long)]
    dev_mode: bool,
}

#[derive(Clone)]
pub struct AppState {
    pub dev_mode: bool,
    pub db_pool: DbPool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "info,tower_http=info".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    dotenvy::dotenv().ok();

    let pool = db::create_pool()?;
    db::run_migrations(&pool)?;

    let app_state = Arc::new(AppState { dev_mode: args.dev_mode, db_pool: pool });

    let app = Router::new()
        .route("/api/health", get(handlers::health::health))
        .with_state(app_state)
        // ws-bridge handler returns MethodRouter<()>, add after .with_state()
        .route(shared::AppSocket::PATH, handlers::websocket::handler())
        .fallback(axum::routing::get(embedded_assets::serve_embedded_frontend))
        .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}
```

Graceful shutdown handles both SIGTERM (Docker/k8s) and Ctrl+C:

```rust
async fn shutdown_signal() {
    let ctrl_c = async { tokio::signal::ctrl_c().await.unwrap(); };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .unwrap().recv().await;
    };
    tokio::select! {
        _ = ctrl_c => tracing::info!("Received Ctrl+C, shutting down..."),
        _ = terminate => tracing::info!("Received SIGTERM, shutting down..."),
    }
}
```

---

## Pattern 7: Docker + .env Injection

The Dockerfile takes a **pre-built binary** (frontend already embedded). No multi-stage Rust build needed because CI compiles it:

```dockerfile
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates libpq5 libssl3 curl \
    && rm -rf /var/lib/apt/lists/*
COPY build-output/backend /app/backend
RUN useradd -m -u 1001 -s /bin/bash appuser && chown -R appuser:appuser /app
USER appuser
EXPOSE 3000
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:3000/api/health || exit 1
CMD ["/app/backend"]
```

`docker-compose.yml` uses `${VAR:-default}` so a `.env` file is picked up automatically:

```yaml
services:
  db:
    image: postgres:16-alpine
    environment:
      POSTGRES_DB: skeleton
      POSTGRES_USER: skeleton
      POSTGRES_PASSWORD: dev_password
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U skeleton"]

  backend:
    build: .
    depends_on:
      db: { condition: service_healthy }
    environment:
      DATABASE_URL: "postgresql://skeleton:dev_password@db:5432/skeleton"
      SESSION_SECRET: "${SESSION_SECRET:-dev-secret-change-in-production}"
```

---

## Pattern 8: CI Pipeline (`.github/workflows/`)

**ci.yml** runs these parallel jobs on every push/PR to main:

| Job | What it does |
|-----|-------------|
| **lint** | `./scripts/check-migration-names.sh` |
| **audit** | `cargo install cargo-audit && cargo audit` |
| **fmt** | `cargo fmt --all --check` |
| **clippy** | Build frontend, then `cargo clippy --workspace --all-targets` |
| **test** | Build frontend, then `cargo test --workspace` |

Clippy and test both **build the frontend first** because `rust-embed` needs `frontend/dist/` to exist at compile time.

**container.yml** builds a release binary and Docker image on every push/PR to main:

| Job | What it does |
|-----|-------------|
| **build-release** | `trunk build --release` + `cargo build --release -p backend`, uploads binary as artifact |
| **container** | Downloads binary, builds Docker image with buildx layer caching |

- On PR: container builds but does **not** push (validates the image)
- On merge to main: pushes to GHCR with `latest` and git SHA tags
- Docker layer caching via `cache-from: type=gha` / `cache-to: type=gha,mode=max` avoids rebuilding unchanged layers
- Release binary is uploaded as an artifact (`backend-linux-x86_64`) so the container job doesn't recompile

---

## Pattern 9: Branch Protection + Automerge Setup

Run these once after creating the repo to enforce required checks and squash-only merges.

**Restrict merge strategies to squash only and enable automerge:**

```sh
gh repo edit \
  --enable-squash-merge \
  --disable-merge-commit \
  --disable-rebase-merge \
  --enable-auto-merge \
  --delete-branch-on-merge
```

**Require all CI checks to pass before merging to main:**

```sh
gh api repos/{owner}/{repo}/branches/main/protection \
  --method PUT \
  --input - <<'EOF'
{
  "required_status_checks": {
    "strict": false,
    "contexts": [
      "Lint Checks",
      "Security Audit",
      "Rustfmt",
      "Clippy",
      "Tests",
      "Build Release Binary",
      "Build Container"
    ]
  },
  "enforce_admins": false,
  "required_pull_request_reviews": null,
  "restrictions": null,
  "required_linear_history": true
}
EOF
```

`strict: false` means PRs don't need to be up-to-date with main before merging (avoids a rebase treadmill on busy repos). `required_linear_history: true` enforces squash commits at the branch level as a backstop.

**Enable automerge on a specific PR** (once all checks are green it merges automatically):

```sh
gh pr merge --auto --squash <PR-number>
```

---

## Stack Reference

| Layer | Crate | Version |
|-------|-------|---------|
| Web framework | `axum` | 0.7 |
| Async runtime | `tokio` | 1 (full) |
| Frontend | `yew` | 0.21 (CSR) |
| WASM bundler | `trunk` | CLI tool |
| Asset embedding | `rust-embed` | 8 |
| Database | `diesel` | 2.2 (postgres, r2d2) |
| Typed WebSockets | `ws-bridge` | 0.1 (server + yew-client) |
| Serialization | `serde` + `serde_json` | 1 |
| CLI args | `clap` | 4 (derive) |
| Env loading | `dotenvy` | 0.15 |
| Logging | `tracing` | 0.1 |
| HTTP middleware | `tower-http` | 0.6 (cors) |
| Cookies | `tower-cookies` | 0.10 |
| Error handling | `anyhow` | 1 |
