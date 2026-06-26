use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    body::Body,
    extract::{ConnectInfo, Request, State},
    http::{header, HeaderValue, Response, StatusCode},
    middleware::Next,
};
use tokio::sync::Mutex;

use crate::client_ip;

const STALE_BUCKET_AFTER: Duration = Duration::from_secs(10 * 60);
const CLEANUP_THRESHOLD: usize = 10_000;

#[derive(Clone)]
pub struct RateLimitState {
    buckets: Arc<Mutex<HashMap<BucketKey, Bucket>>>,
}

impl RateLimitState {
    pub fn new() -> Self {
        Self {
            buckets: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum BucketClass {
    Queue,
    Play,
    Poll,
    General,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct BucketKey {
    ip: IpAddr,
    class: BucketClass,
}

#[derive(Debug, Clone, Copy)]
struct Policy {
    capacity: f64,
    refill_per_second: f64,
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    updated_at: Instant,
}

/// Per-IP token-bucket guard for every HTTP request, including WebSocket
/// upgrades. Returns 429 + Retry-After when the class bucket is empty.
pub async fn enforce(
    State(state): State<RateLimitState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response<Body> {
    let ip = client_ip::resolve_client_ip(req.headers(), peer.ip());
    let class = classify_path(req.uri().path());
    if let Some(retry_after) = check_rate(&state, ip, class).await {
        return too_many_requests(retry_after);
    }
    next.run(req).await
}

async fn check_rate(state: &RateLimitState, ip: IpAddr, class: BucketClass) -> Option<Duration> {
    let policy = policy_for(class);
    let now = Instant::now();
    let mut buckets = state.buckets.lock().await;
    if buckets.len() > CLEANUP_THRESHOLD {
        buckets.retain(|_, bucket| now.duration_since(bucket.updated_at) < STALE_BUCKET_AFTER);
    }
    let bucket = buckets.entry(BucketKey { ip, class }).or_insert(Bucket {
        tokens: policy.capacity,
        updated_at: now,
    });
    bucket.refill(policy, now);
    bucket.try_take(policy, now)
}

fn too_many_requests(retry_after: Duration) -> Response<Body> {
    let seconds = retry_after.as_secs().max(1).to_string();
    Response::builder()
        .status(StatusCode::TOO_MANY_REQUESTS)
        .header(
            header::RETRY_AFTER,
            HeaderValue::from_str(&seconds).unwrap_or_else(|_| HeaderValue::from_static("1")),
        )
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(format!(
            "{{\"error\":\"rate limit exceeded\",\"retry_after_seconds\":{seconds}}}"
        )))
        .expect("static rate limit response is valid")
}

impl Bucket {
    fn refill(&mut self, policy: Policy, now: Instant) {
        let elapsed = now.duration_since(self.updated_at).as_secs_f64();
        self.tokens = (self.tokens + elapsed * policy.refill_per_second).min(policy.capacity);
        self.updated_at = now;
    }

    fn try_take(&mut self, policy: Policy, now: Instant) -> Option<Duration> {
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            None
        } else {
            let missing = 1.0 - self.tokens;
            let seconds = (missing / policy.refill_per_second).ceil().max(1.0);
            self.updated_at = now;
            Some(Duration::from_secs(seconds as u64))
        }
    }
}

fn classify_path(path: &str) -> BucketClass {
    match path {
        "/api/play/register" | "/api/play/queue" | "/ws/agent" => BucketClass::Queue,
        "/api/play/commit" | "/api/play/reveal" | "/api/play/chat" => BucketClass::Play,
        "/api/play/poll" => BucketClass::Poll,
        _ => BucketClass::General,
    }
}

fn policy_for(class: BucketClass) -> Policy {
    match class {
        // Registration/queue/websocket upgrades are the highest-abuse paths:
        // enough for normal use and local multi-client testing, not enough to
        // churn identities or queues at leaderboard-grind rates.
        BucketClass::Queue => Policy {
            capacity: 20.0,
            refill_per_second: 0.2,
        },
        // Commit/reveal already have per-player pacing; this caps aggregate IP
        // spam without breaking two normal players behind one NAT.
        BucketClass::Play => Policy {
            capacity: 30.0,
            refill_per_second: 1.0,
        },
        // Long-poll is expected to be frequent, so it gets a wider bucket.
        BucketClass::Poll => Policy {
            capacity: 60.0,
            refill_per_second: 2.0,
        },
        // Public read/static endpoints are still bounded for coarse DoS control.
        BucketClass::General => Policy {
            capacity: 120.0,
            refill_per_second: 2.0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_play_paths() {
        assert_eq!(classify_path("/api/play/register"), BucketClass::Queue);
        assert_eq!(classify_path("/ws/agent"), BucketClass::Queue);
        assert_eq!(classify_path("/api/play/commit"), BucketClass::Play);
        assert_eq!(classify_path("/api/play/poll"), BucketClass::Poll);
        assert_eq!(classify_path("/api/leaderboard"), BucketClass::General);
    }

    #[test]
    fn bucket_rejects_when_empty_and_reports_retry() {
        let policy = Policy {
            capacity: 2.0,
            refill_per_second: 1.0,
        };
        let now = Instant::now();
        let mut bucket = Bucket {
            tokens: policy.capacity,
            updated_at: now,
        };
        assert_eq!(bucket.try_take(policy, now), None);
        assert_eq!(bucket.try_take(policy, now), None);
        assert_eq!(bucket.try_take(policy, now), Some(Duration::from_secs(1)));
    }

    #[test]
    fn bucket_refills_over_time() {
        let policy = Policy {
            capacity: 2.0,
            refill_per_second: 1.0,
        };
        let now = Instant::now();
        let mut bucket = Bucket {
            tokens: 0.0,
            updated_at: now,
        };
        bucket.refill(policy, now + Duration::from_millis(1500));
        assert!(bucket.tokens >= 1.4 && bucket.tokens <= 1.6);
        assert_eq!(
            bucket.try_take(policy, now + Duration::from_millis(1500)),
            None
        );
    }
}
