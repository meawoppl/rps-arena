use axum::Json;
use shared::HealthResponse;

/// Crate version, baked in at compile time.
const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Short git SHA, emitted by `build.rs` (`"unknown"` outside a checkout).
const GIT_SHA: &str = env!("GIT_SHA");

pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        version: VERSION.to_string(),
        git_sha: GIT_SHA.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn health_reports_status_and_build_metadata() {
        let Json(body) = health().await;
        assert_eq!(body.status, "ok");
        // Version is the crate version, never empty.
        assert_eq!(body.version, env!("CARGO_PKG_VERSION"));
        assert!(!body.version.is_empty());
        // git_sha is always populated by build.rs (a real SHA or "unknown").
        assert!(!body.git_sha.is_empty());
    }
}
