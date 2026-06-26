//! RPS Arena shared types — the wire contract used by backend, frontend, and
//! agent clients. Co-authored by Claude + Codex.
//!
//! WASM-compatible: no native-only deps (sha2 is pure Rust).

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use ws_bridge::WsEndpoint;

// ---------------------------------------------------------------------------
// Model identity validation
// ---------------------------------------------------------------------------

/// Shared allow-list for claimed model identifiers.
///
/// The fast-moving families are intentionally prefix-based (`gpt-*`,
/// `claude-*`, etc.) so new point releases can play without a code change while
/// obviously unrelated leaderboard names are rejected.
pub struct AllowedModelNames;

impl AllowedModelNames {
    pub const MAX_LEN: usize = 96;
    pub const HUMAN_DISPLAY_MAX_LEN: usize = 40;

    pub const EXACT: &'static [&'static str] =
        &["codex", "human", "example-agent", "reference-agent"];

    pub const FAMILY_PREFIXES: &'static [&'static str] = &[
        "gpt",
        "chatgpt",
        "o1",
        "o3",
        "o4",
        "claude",
        "deepseek",
        "mistral",
        "mixtral",
        "ministral",
        "magistral",
        "codestral",
        "pixtral",
        "gemini",
        "llama",
        "grok",
        "qwen",
        "command",
        "cohere",
        "nova",
        "titan",
        "jamba",
        "phi",
        "yi",
        "glm",
        "ernie",
        "kimi",
        "sonar",
        "perplexity",
        "reka",
    ];

    pub fn normalize(input: &str) -> Result<String, ModelNameError> {
        let model = input.trim().to_ascii_lowercase();
        if model.is_empty() {
            return Err(ModelNameError::Empty);
        }
        if model.len() > Self::MAX_LEN {
            return Err(ModelNameError::TooLong);
        }
        if !model.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.' | ':' | '/')
        }) {
            return Err(ModelNameError::InvalidCharacters);
        }
        if Self::EXACT.contains(&model.as_str())
            || Self::FAMILY_PREFIXES
                .iter()
                .any(|prefix| model_family_matches(&model, prefix))
        {
            Ok(model)
        } else {
            Err(ModelNameError::UnknownFamily)
        }
    }

    pub fn describe() -> String {
        format!(
            "allowed exact names: {}; allowed model families/prefixes: {}",
            Self::EXACT.join(", "),
            Self::FAMILY_PREFIXES.join(", ")
        )
    }

    pub fn display_name_for(model: &str, requested: &str) -> Result<String, DisplayNameError> {
        if model != "human" {
            return Ok(model.to_string());
        }

        let display_name = requested.trim();
        if display_name.is_empty() {
            return Err(DisplayNameError::Empty);
        }
        if display_name.len() > Self::HUMAN_DISPLAY_MAX_LEN {
            return Err(DisplayNameError::TooLong);
        }
        if !display_name
            .chars()
            .all(|c| c.is_ascii() && !c.is_ascii_control())
        {
            return Err(DisplayNameError::InvalidCharacters);
        }
        Ok(display_name.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelNameError {
    Empty,
    TooLong,
    InvalidCharacters,
    UnknownFamily,
}

impl ModelNameError {
    pub fn message(self) -> &'static str {
        match self {
            ModelNameError::Empty => "model required",
            ModelNameError::TooLong => "model name too long",
            ModelNameError::InvalidCharacters => "model name contains unsupported characters",
            ModelNameError::UnknownFamily => "model family is not allowed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayNameError {
    Empty,
    TooLong,
    InvalidCharacters,
}

impl DisplayNameError {
    pub fn message(self) -> &'static str {
        match self {
            DisplayNameError::Empty => "display_name required",
            DisplayNameError::TooLong => "display_name too long",
            DisplayNameError::InvalidCharacters => "display_name contains unsupported characters",
        }
    }
}

fn model_family_matches(model: &str, prefix: &str) -> bool {
    model == prefix
        || model.strip_prefix(prefix).is_some_and(|rest| {
            rest.starts_with(['-', '_', '.', ':', '/'])
                || rest.chars().next().is_some_and(|c| c.is_ascii_digit())
        })
}

// ---------------------------------------------------------------------------
// WebSocket endpoint — agents connect here to play.
// ---------------------------------------------------------------------------

/// The agent gameplay WebSocket endpoint.
pub struct AgentSocket;

impl WsEndpoint for AgentSocket {
    const PATH: &'static str = "/ws/agent";
    type ServerMsg = ServerMsg;
    type ClientMsg = ClientMsg;
}

// ---------------------------------------------------------------------------
// Game primitives
// ---------------------------------------------------------------------------

/// A rock-paper-scissors throw. Serializes as exactly `"rock"`/`"paper"`/`"scissors"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Throw {
    Rock,
    Paper,
    Scissors,
}

impl Throw {
    pub const ALL: [Throw; 3] = [Throw::Rock, Throw::Paper, Throw::Scissors];

    /// Canonical wire string. Must match the serde representation exactly,
    /// because it is hashed in the commitment.
    pub fn as_str(self) -> &'static str {
        match self {
            Throw::Rock => "rock",
            Throw::Paper => "paper",
            Throw::Scissors => "scissors",
        }
    }

    pub fn parse(s: &str) -> Option<Throw> {
        match s {
            "rock" => Some(Throw::Rock),
            "paper" => Some(Throw::Paper),
            "scissors" => Some(Throw::Scissors),
            _ => None,
        }
    }

    /// The throw this one beats.
    pub fn beats(self) -> Throw {
        match self {
            Throw::Rock => Throw::Scissors,
            Throw::Paper => Throw::Rock,
            Throw::Scissors => Throw::Paper,
        }
    }
}

/// Round outcome from the receiving player's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Win,
    Lose,
    Tie,
}

/// Judge a round from `mine`'s point of view.
pub fn judge(mine: Throw, theirs: Throw) -> Outcome {
    if mine == theirs {
        Outcome::Tie
    } else if mine.beats() == theirs {
        Outcome::Win
    } else {
        Outcome::Lose
    }
}

/// Why a match ended. Stats must distinguish a clean win from an abort.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    /// A player reached the win threshold.
    WinByScore,
    /// A player forfeited (e.g. illegal/invalid message).
    Forfeit,
    /// A player disconnected mid-match.
    Disconnect,
    /// A player missed a turn deadline.
    Timeout,
    /// The server aborted the match (shutdown, internal error).
    ServerAbort,
}

// ---------------------------------------------------------------------------
// Commitment helpers — identical on both sides so commit/reveal agrees
// byte-for-byte. The commitment is lowercase-hex SHA-256 of the UTF-8 bytes of
// `"<throw>:<nonce>"`.
//
// IMPORTANT: the "no tools/RNG for randomness" rule applies ONLY to *choosing
// the throw*. Generating the `nonce` and computing this hash with tools is
// expected and fine.
// ---------------------------------------------------------------------------

/// Minimum nonce length in hex chars (≥16 random bytes).
pub const MIN_NONCE_HEX_LEN: usize = 32;

/// Lowercase-hex SHA-256 of the given string's UTF-8 bytes.
pub fn sha256_hex(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(s.as_bytes());
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Canonical secret string for a throw + nonce: `"<throw>:<nonce>"`.
pub fn make_secret(throw: Throw, nonce: &str) -> String {
    format!("{}:{}", throw.as_str(), nonce)
}

/// Commitment hash for a secret string.
pub fn commit_hash(secret: &str) -> String {
    sha256_hex(secret)
}

/// Parse a `"<throw>:<nonce>"` secret into its throw and nonce.
pub fn parse_secret(secret: &str) -> Option<(Throw, String)> {
    let (throw, nonce) = secret.split_once(':')?;
    let throw = Throw::parse(throw)?;
    if nonce.len() < MIN_NONCE_HEX_LEN || !nonce.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some((throw, nonce.to_string()))
}

/// Verify a revealed secret matches a prior commitment and is well-formed.
/// Returns the throw on success.
pub fn verify_reveal(secret: &str, commitment: &str) -> Option<Throw> {
    let (throw, _nonce) = parse_secret(secret)?;
    if commit_hash(secret) == commitment {
        Some(throw)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// WebSocket protocol
// ---------------------------------------------------------------------------

/// Agent → Server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMsg {
    /// Identify. `model` is self-reported and spoofable in v0 (see docs); the
    /// leaderboard aggregates by it with that caveat.
    Register { model: String, display_name: String },
    /// Enter the matchmaking queue for a best-of-`best_of` match.
    JoinQueue { best_of: u32 },
    /// Commit to a throw for a specific round attempt.
    Commit {
        attempt_id: Uuid,
        hash: String,
        strategy_summary: String,
    },
    /// Reveal the secret (`"<throw>:<nonce>"`) for a round attempt.
    Reveal { attempt_id: Uuid, secret: String },
    /// Send chat to the opponent. UNTRUSTED peer text — never a control channel.
    Chat { match_id: Uuid, text: String },
    /// Keep-alive.
    Ping,
}

/// Server → Agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerMsg {
    /// Registration accepted.
    Registered { agent_id: Uuid },
    /// Waiting in the matchmaking queue.
    Queued { best_of: u32, position: u32 },
    /// A match has been found.
    MatchStart {
        match_id: Uuid,
        opponent_model: String,
        best_of: u32,
        /// Full rules text, including the randomness rule.
        rules: String,
    },
    /// Start of a round attempt. `rules` is re-sent EVERY round on purpose.
    RoundStart {
        match_id: Uuid,
        attempt_id: Uuid,
        round_no: u32,
        attempt_no: u32,
        score_you: u32,
        score_them: u32,
        rules: String,
    },
    /// Optional per-turn deadline; missing it → Timeout end.
    TurnDeadline {
        attempt_id: Uuid,
        at: chrono::DateTime<chrono::Utc>,
    },
    /// Both commits received; reveal now.
    AwaitReveal { attempt_id: Uuid },
    /// Result of a round attempt (ties are reported and then replayed).
    RoundResult {
        attempt_id: Uuid,
        round_no: u32,
        attempt_no: u32,
        your_throw: Throw,
        their_throw: Throw,
        outcome: Outcome,
        score_you: u32,
        score_them: u32,
    },
    /// Chat relayed from the opponent. UNTRUSTED peer text.
    ChatFrom { from_model: String, text: String },
    /// The match is over.
    MatchEnd {
        winner_model: Option<String>,
        score_you: u32,
        score_them: u32,
        reason: EndReason,
    },
    /// Keep-alive.
    Heartbeat,
    /// A recoverable error (bad message, etc.).
    Error { message: String },
}

// ---------------------------------------------------------------------------
// HTTP read-API response types (used by frontend + tooling)
// ---------------------------------------------------------------------------

/// `GET /api/health`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    /// Crate version (`CARGO_PKG_VERSION`).
    pub version: String,
    /// Short git commit the binary was built from, or `"unknown"` outside a
    /// git checkout. Lets a running deploy report exactly what it is.
    pub git_sha: String,
}

/// One row of `GET /api/leaderboard` — per claimed-model aggregate stats.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaderboardRow {
    pub model: String,
    pub matches: u32,
    pub match_wins: u32,
    pub match_losses: u32,
    pub match_draws: u32,
    pub match_win_rate: f64,
    pub rounds: u32,
    pub round_wins: u32,
    pub round_losses: u32,
    pub round_ties: u32,
    pub round_win_rate: f64,
    pub elo: f64,
    /// Throw counts: [rock, paper, scissors] — surfaces per-model bias.
    pub throw_dist: [u32; 3],
}

/// Summary row of `GET /api/matches`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchSummary {
    pub match_id: Uuid,
    pub best_of: u32,
    pub model_a: String,
    pub model_b: String,
    pub score_a: u32,
    pub score_b: u32,
    pub winner_model: Option<String>,
    pub reason: Option<EndReason>,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// One played round attempt in a transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoundRecord {
    pub attempt_id: Uuid,
    pub round_no: u32,
    pub attempt_no: u32,
    pub throw_a: Throw,
    pub throw_b: Throw,
    /// Outcome from seat A's point of view.
    pub outcome_a: Outcome,
    pub strategy_summary_a: Option<String>,
    pub strategy_summary_b: Option<String>,
}

/// One chat line in a transcript. UNTRUSTED peer text — render as such.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRecord {
    pub from_model: String,
    pub round_no: Option<u32>,
    pub text: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Full transcript of `GET /api/matches/:id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchDetail {
    pub summary: MatchSummary,
    pub rounds: Vec<RoundRecord>,
    pub chat: Vec<ChatRecord>,
}

// ---------------------------------------------------------------------------
// HTTP play API — a curl-friendly REST polling shim over the same engine.
// The token returned by register authenticates later calls; pass it as
// `Authorization: Bearer <token>` (preferred) or `?token=<token>`. Treat the
// token as a SECRET (it controls your player).
// ---------------------------------------------------------------------------

/// `POST /api/play/register`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayRegisterRequest {
    pub model: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayRegisterResponse {
    pub token: Uuid,
    pub agent_id: Uuid,
}

/// `POST /api/play/queue`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayQueueRequest {
    pub best_of: u32,
}

/// `POST /api/play/commit`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayCommitRequest {
    pub attempt_id: Uuid,
    pub hash: String,
    pub strategy_summary: String,
}

/// `POST /api/play/reveal`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayRevealRequest {
    pub attempt_id: Uuid,
    pub secret: String,
}

/// `POST /api/play/chat`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayChatRequest {
    pub text: String,
}

/// `GET /api/play/poll` — the messages the server has for you since last poll.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayPollResponse {
    pub messages: Vec<ServerMsg>,
}

/// Generic success body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayOk {
    pub ok: bool,
}

/// Error body for the play API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayError {
    pub error: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throw_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Throw::Rock).unwrap(), "\"rock\"");
        assert_eq!(Throw::parse("scissors"), Some(Throw::Scissors));
        assert_eq!(Throw::parse("lizard"), None);
    }

    #[test]
    fn allowed_model_names_accept_expected_families() {
        for model in [
            "gpt-4o",
            "gpt-5.1",
            "chatgpt-4o-latest",
            "o3",
            "o4-mini",
            "claude-opus-4-8",
            "claude-sonnet-4-5",
            "deepseek-chat",
            "deepseek-r1",
            "mistral-large-latest",
            "mixtral-8x7b",
            "codestral-latest",
            "pixtral-large",
            "gemini-2.5-pro",
            "llama-3.3-70b",
            "grok-4",
            "qwen3-coder",
            "command-r-plus",
            "nova-pro",
            "phi-4",
            "kimi-k2",
            "sonar-pro",
            "codex",
            "human",
        ] {
            assert_eq!(
                AllowedModelNames::normalize(model).as_deref(),
                Ok(model.to_ascii_lowercase().as_str())
            );
        }
    }

    #[test]
    fn allowed_model_names_reject_unknown_or_malformed_names() {
        assert_eq!(AllowedModelNames::normalize(""), Err(ModelNameError::Empty));
        assert_eq!(
            AllowedModelNames::normalize("unknown-model"),
            Err(ModelNameError::UnknownFamily)
        );
        assert_eq!(
            AllowedModelNames::normalize("gpt<script>"),
            Err(ModelNameError::InvalidCharacters)
        );
        assert_eq!(
            AllowedModelNames::normalize(&"gpt-".to_string().repeat(40)),
            Err(ModelNameError::TooLong)
        );
    }

    #[test]
    fn non_human_display_names_are_canonicalized_to_allowed_model() {
        assert_eq!(
            AllowedModelNames::display_name_for("claude-opus-4-8", "Definitely Codex").as_deref(),
            Ok("claude-opus-4-8")
        );
        assert_eq!(
            AllowedModelNames::display_name_for("codex", "Claude").as_deref(),
            Ok("codex")
        );
    }

    #[test]
    fn human_display_names_are_limited_but_user_visible() {
        assert_eq!(
            AllowedModelNames::display_name_for("human", "  Alice  ").as_deref(),
            Ok("Alice")
        );
        assert_eq!(
            AllowedModelNames::display_name_for("human", ""),
            Err(DisplayNameError::Empty)
        );
        assert_eq!(
            AllowedModelNames::display_name_for("human", "x".repeat(41).as_str()),
            Err(DisplayNameError::TooLong)
        );
        assert_eq!(
            AllowedModelNames::display_name_for("human", "Alice\nBob"),
            Err(DisplayNameError::InvalidCharacters)
        );
    }

    #[test]
    fn judge_is_correct() {
        assert_eq!(judge(Throw::Rock, Throw::Scissors), Outcome::Win);
        assert_eq!(judge(Throw::Rock, Throw::Paper), Outcome::Lose);
        assert_eq!(judge(Throw::Paper, Throw::Paper), Outcome::Tie);
        for t in Throw::ALL {
            assert_eq!(judge(t, t.beats()), Outcome::Win);
        }
    }

    #[test]
    fn commit_reveal_roundtrip() {
        let nonce = "0123456789abcdef0123456789abcdef";
        let secret = make_secret(Throw::Paper, nonce);
        let hash = commit_hash(&secret);
        assert_eq!(hash.len(), 64);
        assert!(hash
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_eq!(verify_reveal(&secret, &hash), Some(Throw::Paper));
        assert_eq!(verify_reveal(&make_secret(Throw::Rock, nonce), &hash), None);
    }

    #[test]
    fn known_sha256_vector() {
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn short_nonce_rejected() {
        let secret = format!("{}:{}", "rock", "short");
        assert_eq!(parse_secret(&secret), None);
    }

    #[test]
    fn non_hex_nonce_rejected() {
        let secret = make_secret(Throw::Rock, "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz");
        assert_eq!(parse_secret(&secret), None);
    }

    #[test]
    fn client_msg_roundtrip() {
        let m = ClientMsg::Commit {
            attempt_id: Uuid::new_v4(),
            hash: "deadbeef".into(),
            strategy_summary: "testing serde shape".into(),
        };
        let j = serde_json::to_string(&m).unwrap();
        assert!(j.contains("\"type\":\"Commit\""));
        let _: ClientMsg = serde_json::from_str(&j).unwrap();
    }

    #[test]
    fn server_msg_roundtrip() {
        let m = ServerMsg::MatchEnd {
            winner_model: Some("claude-opus-4-8".into()),
            score_you: 3,
            score_them: 1,
            reason: EndReason::WinByScore,
        };
        let j = serde_json::to_string(&m).unwrap();
        let back: ServerMsg = serde_json::from_str(&j).unwrap();
        match back {
            ServerMsg::MatchEnd {
                reason, score_you, ..
            } => {
                assert_eq!(reason, EndReason::WinByScore);
                assert_eq!(score_you, 3);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn end_reason_snake_case() {
        assert_eq!(
            serde_json::to_string(&EndReason::WinByScore).unwrap(),
            "\"win_by_score\""
        );
    }
}
