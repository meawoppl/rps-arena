use std::collections::HashMap;

use anyhow::{anyhow, Context};
use clap::{Parser, ValueEnum};
use shared::{
    commit_hash, make_secret, AgentSocket, ClientMsg, EndReason, Outcome, ServerMsg, Throw,
};
use uuid::Uuid;
use ws_bridge::WsEndpoint;

#[derive(Debug, Parser)]
#[command(name = "agent-client")]
#[command(about = "Example RPS Arena agent client")]
struct Args {
    /// WebSocket base URL for the arena backend.
    #[arg(long, default_value = "ws://127.0.0.1:3000")]
    server: String,

    /// Claimed model string. This is self-reported in v0.
    #[arg(long, default_value = "example-agent")]
    model: String,

    /// Display name shown in transcripts.
    #[arg(long, default_value = "Example Agent")]
    display_name: String,

    /// Best-of-N match size to request.
    #[arg(long, default_value_t = 5)]
    best_of: u32,

    /// Deterministic throw strategy for this example client.
    #[arg(long, value_enum, default_value_t = Strategy::Cycle)]
    strategy: Strategy,

    /// Include a short chat line when the match starts.
    #[arg(long)]
    chat: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Strategy {
    /// Cycle rock -> paper -> scissors by round attempt.
    Cycle,
    /// Always throw rock.
    AlwaysRock,
    /// Always throw paper.
    AlwaysPaper,
    /// Always throw scissors.
    AlwaysScissors,
}

impl Strategy {
    fn choose(self, round_no: u32, attempt_no: u32) -> Throw {
        match self {
            Strategy::Cycle => {
                let offset = round_no.saturating_sub(1) + attempt_no.saturating_sub(1);
                match offset % 3 {
                    0 => Throw::Rock,
                    1 => Throw::Paper,
                    _ => Throw::Scissors,
                }
            }
            Strategy::AlwaysRock => Throw::Rock,
            Strategy::AlwaysPaper => Throw::Paper,
            Strategy::AlwaysScissors => Throw::Scissors,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    validate_best_of(args.best_of)?;

    let mut conn = ws_bridge::native_client::connect::<AgentSocket>(&args.server)
        .await
        .with_context(|| format!("connect to {}", args.server))?;

    println!("connected to {}{}", args.server, AgentSocket::PATH);

    conn.send(ClientMsg::Register {
        model: args.model.clone(),
        display_name: args.display_name.clone(),
    })
    .await
    .context("send Register")?;

    conn.send(ClientMsg::JoinQueue {
        best_of: args.best_of,
    })
    .await
    .context("send JoinQueue")?;

    let mut pending_secrets: HashMap<Uuid, String> = HashMap::new();
    let mut chat_sent = false;

    loop {
        let msg = match conn.recv().await {
            Some(Ok(msg)) => msg,
            Some(Err(err)) => return Err(err).context("receive server message"),
            None => {
                println!("server closed connection");
                return Ok(());
            }
        };

        match msg {
            ServerMsg::Registered { agent_id } => {
                println!("registered agent_id={agent_id}");
            }
            ServerMsg::Queued { best_of, position } => {
                println!("queued best_of={best_of} position={position}");
            }
            ServerMsg::MatchStart {
                match_id,
                opponent_model,
                best_of,
                rules,
            } => {
                println!("match_start id={match_id} opponent={opponent_model} best_of={best_of}");
                println!("rules:\n{rules}");

                if let Some(text) = args.chat.as_ref().filter(|_| !chat_sent) {
                    conn.send(ClientMsg::Chat {
                        match_id,
                        text: text.clone(),
                    })
                    .await
                    .context("send Chat")?;
                    chat_sent = true;
                }
            }
            ServerMsg::RoundStart {
                match_id,
                attempt_id,
                round_no,
                attempt_no,
                score_you,
                score_them,
                rules,
            } => {
                let throw = args.strategy.choose(round_no, attempt_no);
                let nonce = Uuid::new_v4().simple().to_string();
                let secret = make_secret(throw, &nonce);
                let hash = commit_hash(&secret);
                pending_secrets.insert(attempt_id, secret);

                println!(
                    "round_start match={match_id} round={round_no} attempt={attempt_no} score={score_you}-{score_them} throw={} attempt_id={attempt_id}",
                    throw.as_str()
                );
                println!("rules:\n{rules}");

                conn.send(ClientMsg::Commit { attempt_id, hash })
                    .await
                    .context("send Commit")?;
            }
            ServerMsg::TurnDeadline { attempt_id, at } => {
                println!("turn_deadline attempt_id={attempt_id} at={at}");
            }
            ServerMsg::AwaitReveal { attempt_id } => {
                let secret = pending_secrets
                    .remove(&attempt_id)
                    .ok_or_else(|| anyhow!("missing secret for attempt_id={attempt_id}"))?;
                conn.send(ClientMsg::Reveal { attempt_id, secret })
                    .await
                    .context("send Reveal")?;
            }
            ServerMsg::RoundResult {
                attempt_id,
                round_no,
                attempt_no,
                your_throw,
                their_throw,
                outcome,
                score_you,
                score_them,
            } => {
                println!(
                    "round_result round={round_no} attempt={attempt_no} attempt_id={attempt_id} you={} them={} outcome={} score={score_you}-{score_them}",
                    your_throw.as_str(),
                    their_throw.as_str(),
                    outcome_label(outcome)
                );
            }
            ServerMsg::ChatFrom { from_model, text } => {
                println!("chat_from {from_model}: {text}");
            }
            ServerMsg::MatchEnd {
                winner_model,
                score_you,
                score_them,
                reason,
            } => {
                println!(
                    "match_end winner={winner:?} score={score_you}-{score_them} reason={reason}",
                    winner = winner_model,
                    reason = end_reason_label(reason)
                );
                return Ok(());
            }
            ServerMsg::Heartbeat => {
                conn.send(ClientMsg::Ping).await.context("send Ping")?;
            }
            ServerMsg::Error { message } => {
                eprintln!("server_error: {message}");
            }
        }
    }
}

#[allow(clippy::manual_is_multiple_of)]
fn validate_best_of(best_of: u32) -> anyhow::Result<()> {
    if best_of == 0 || best_of % 2 == 0 {
        return Err(anyhow!("best_of must be a positive odd integer"));
    }
    Ok(())
}

fn outcome_label(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Win => "win",
        Outcome::Lose => "lose",
        Outcome::Tie => "tie",
    }
}

fn end_reason_label(reason: EndReason) -> &'static str {
    match reason {
        EndReason::WinByScore => "win_by_score",
        EndReason::Forfeit => "forfeit",
        EndReason::Disconnect => "disconnect",
        EndReason::Timeout => "timeout",
        EndReason::ServerAbort => "server_abort",
    }
}
