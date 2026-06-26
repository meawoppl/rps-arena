mod client_ip;
mod db;
mod db_ops;
mod embedded_assets;
mod game;
mod handlers;
mod models;
mod rate_limit;
mod rules;
mod schema;

use crate::db::DbPool;
use crate::game::Matchmaker;
use anyhow::{Context, Result};
use axum::{
    http::{
        header::{AUTHORIZATION, CONTENT_TYPE},
        HeaderValue, Method,
    },
    middleware,
    routing::{get, post},
    Router,
};
use clap::Parser;
use std::{env, net::SocketAddr, sync::Arc};
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use ws_bridge::WsEndpoint;

#[derive(Parser, Debug, Clone)]
#[command(name = "backend")]
#[command(about = "Backend server")]
struct Args {
    /// Enable development mode (relaxed config requirements)
    #[arg(long)]
    dev_mode: bool,
}

#[derive(Clone)]
pub struct AppState {
    pub dev_mode: bool,
    pub db_pool: DbPool,
    pub matchmaker: Matchmaker,
    pub http_sessions: handlers::play::HttpSessions,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    if args.dev_mode {
        tracing::warn!("DEV MODE ENABLED");
    }

    // Load .env file if present
    dotenvy::dotenv().ok();

    // Create database pool and run migrations
    let pool = db::create_pool()?;

    tracing::info!("Running database migrations...");
    match db::run_migrations(&pool) {
        Ok(applied) => {
            if applied.is_empty() {
                tracing::info!("Database is up to date");
            } else {
                for m in &applied {
                    tracing::info!("Applied migration: {}", m);
                }
            }
        }
        Err(e) => {
            tracing::error!("Failed to run migrations: {}", e);
            return Err(e);
        }
    }

    let matchmaker = Matchmaker::new(pool.clone());
    let http_sessions = handlers::play::new_sessions();
    handlers::play::spawn_reaper(http_sessions.clone());
    let rate_limits = rate_limit::RateLimitState::new();
    let app_state = Arc::new(AppState {
        dev_mode: args.dev_mode,
        db_pool: pool,
        matchmaker,
        http_sessions,
    });

    let cors = cors_layer(args.dev_mode)?;

    // Router
    let app = Router::new()
        .route("/api/health", get(handlers::health::health))
        .route("/api/leaderboard", get(handlers::read_api::leaderboard))
        .route("/api/matches", get(handlers::read_api::list_matches))
        .route("/api/matches/:id", get(handlers::read_api::get_match))
        .route("/api/play/register", post(handlers::play::register))
        .route("/api/play/queue", post(handlers::play::queue))
        .route("/api/play/commit", post(handlers::play::commit))
        .route("/api/play/reveal", post(handlers::play::reveal))
        .route("/api/play/chat", post(handlers::play::chat))
        .route("/api/play/poll", get(handlers::play::poll))
        .with_state(app_state.clone())
        .route(
            shared::AgentSocket::PATH,
            handlers::websocket::handler(app_state.clone()),
        )
        .fallback(axum::routing::get(embedded_assets::serve_embedded_frontend))
        .layer(middleware::from_fn(client_ip::capture_client_ip))
        .layer(middleware::from_fn_with_state(
            rate_limits,
            rate_limit::enforce,
        ))
        .layer(cors);

    // Bind and serve
    let host = env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let port = env::var("PORT").unwrap_or_else(|_| "3000".to_string());
    let addr = format!("{}:{}", host, port);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("Listening on {}", listener.local_addr()?);

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    Ok(())
}

fn cors_layer(dev_mode: bool) -> Result<CorsLayer> {
    let layer = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([AUTHORIZATION, CONTENT_TYPE]);

    match env::var("CORS_ALLOWED_ORIGINS") {
        Ok(raw) => {
            let raw = raw.trim();
            if raw == "*" {
                anyhow::ensure!(
                    dev_mode,
                    "CORS_ALLOWED_ORIGINS=* is only allowed with --dev-mode"
                );
                return Ok(layer.allow_origin(Any));
            }

            let origins = parse_cors_origins(raw)?;
            Ok(layer.allow_origin(AllowOrigin::list(origins)))
        }
        Err(env::VarError::NotPresent) => Ok(layer),
        Err(env::VarError::NotUnicode(_)) => {
            anyhow::bail!("CORS_ALLOWED_ORIGINS must be valid UTF-8")
        }
    }
}

fn parse_cors_origins(raw: &str) -> Result<Vec<HeaderValue>> {
    let origins: Vec<HeaderValue> = raw
        .split(',')
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
        .map(|origin| {
            HeaderValue::from_str(origin).with_context(|| format!("invalid CORS origin `{origin}`"))
        })
        .collect::<Result<_>>()?;

    anyhow::ensure!(
        !origins.is_empty(),
        "CORS_ALLOWED_ORIGINS must contain at least one origin"
    );
    Ok(origins)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("Received Ctrl+C, shutting down..."),
        _ = terminate => tracing::info!("Received SIGTERM, shutting down..."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_comma_separated_cors_origins() {
        let origins = parse_cors_origins("https://rps.example, http://localhost:8080").unwrap();

        assert_eq!(origins.len(), 2);
        assert_eq!(origins[0], HeaderValue::from_static("https://rps.example"));
        assert_eq!(
            origins[1],
            HeaderValue::from_static("http://localhost:8080")
        );
    }

    #[test]
    fn rejects_empty_cors_origin_list() {
        assert!(parse_cors_origins(" , ").is_err());
    }
}
