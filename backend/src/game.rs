//! Match engine: matchmaking queue + the per-match state machine.
//!
//! Each connection is represented by a [`PlayerConn`] (an outbound sender + an
//! inbound receiver, fed by the WebSocket IO loop in `handlers::websocket`).
//! [`Matchmaker::enqueue`] pairs two waiting players and spawns [`run_match`],
//! which owns both players for the life of the match.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};
use uuid::Uuid;

use shared::{judge, verify_reveal, ClientMsg, EndReason, Outcome, ServerMsg, Throw};

use crate::db::DbPool;
use crate::db_ops::{self, SeatInfo};
use crate::rules;

/// A connected player from the engine's point of view.
pub struct PlayerConn {
    pub agent_id: Uuid,
    pub player_id: Uuid,
    pub model: String,
    pub display_name: String,
    pub out: mpsc::UnboundedSender<ServerMsg>,
    pub inbox: mpsc::UnboundedReceiver<ClientMsg>,
}

impl PlayerConn {
    fn send(&self, m: ServerMsg) {
        let _ = self.out.send(m);
    }
}

/// Clamp a requested best-of to a sane, odd value in `1..=101`.
#[allow(clippy::manual_is_multiple_of)] // `is_multiple_of` is too new for some toolchains
pub fn sanitize_best_of(requested: u32) -> u32 {
    let n = requested.clamp(1, 101);
    if n % 2 == 0 {
        n + 1
    } else {
        n
    }
}

/// Matchmaking queue, keyed by requested best-of.
#[derive(Clone)]
pub struct Matchmaker {
    queue: Arc<Mutex<HashMap<u32, VecDeque<PlayerConn>>>>,
    pool: DbPool,
}

impl Matchmaker {
    pub fn new(pool: DbPool) -> Self {
        Self {
            queue: Arc::new(Mutex::new(HashMap::new())),
            pool,
        }
    }

    /// Enqueue a player. If an opponent waits for the same best-of, pair them
    /// and spawn the match; otherwise hold the player in the queue.
    pub async fn enqueue(&self, best_of: u32, player: PlayerConn) {
        let mut q = self.queue.lock().await;
        let dq = q.entry(best_of).or_default();
        if let Some(opponent) = dq.pop_front() {
            drop(q);
            let pool = self.pool.clone();
            tokio::spawn(async move { run_match(pool, best_of, opponent, player).await });
        } else {
            let pos = dq.len() as u32 + 1;
            player.send(ServerMsg::Queued {
                best_of,
                position: pos,
            });
            dq.push_back(player);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Commit,
    Reveal,
}

enum Flow {
    Continue,
    Abort(EndReason),
}

enum PhaseOut {
    /// (seat-A value, seat-B value) — hashes for Commit, secrets for Reveal.
    Both(String, String),
    /// `winner_seat` is the non-offending player.
    Abort { winner_seat: u8, reason: EndReason },
}

fn outcome_str(o: Outcome) -> &'static str {
    match o {
        Outcome::Win => "win",
        Outcome::Lose => "lose",
        Outcome::Tie => "tie",
    }
}

fn invert(o: Outcome) -> Outcome {
    match o {
        Outcome::Win => Outcome::Lose,
        Outcome::Lose => Outcome::Win,
        Outcome::Tie => Outcome::Tie,
    }
}

fn end_reason_str(r: EndReason) -> &'static str {
    match r {
        EndReason::WinByScore => "win_by_score",
        EndReason::Forfeit => "forfeit",
        EndReason::Disconnect => "disconnect",
        EndReason::Timeout => "timeout",
        EndReason::ServerAbort => "server_abort",
    }
}

/// Process one inbound message during a phase; chat/ping are handled inline and
/// relayed, the expected commit/reveal fills `slot`, anything else aborts.
#[allow(clippy::too_many_arguments)]
async fn handle_msg(
    pool: &DbPool,
    match_id: Uuid,
    round_no: i32,
    attempt_id: Uuid,
    phase: Phase,
    msg: Option<ClientMsg>,
    me: &PlayerConn,
    opp: &PlayerConn,
    slot: &mut Option<String>,
) -> Flow {
    match msg {
        None => Flow::Abort(EndReason::Disconnect),
        Some(ClientMsg::Chat { text, .. }) => {
            if let Err(e) = db_ops::record_chat(
                pool,
                match_id,
                Some(round_no),
                me.model.clone(),
                text.clone(),
            )
            .await
            {
                tracing::warn!("chat persist failed: {e}");
            }
            opp.send(ServerMsg::ChatFrom {
                from_model: me.model.clone(),
                text,
            });
            Flow::Continue
        }
        Some(ClientMsg::Ping) => {
            me.send(ServerMsg::Heartbeat);
            Flow::Continue
        }
        Some(ClientMsg::Commit {
            attempt_id: aid,
            hash,
        }) if phase == Phase::Commit => {
            if aid == attempt_id {
                *slot = Some(hash);
                Flow::Continue
            } else {
                me.send(ServerMsg::Error {
                    message: "commit for wrong attempt_id".into(),
                });
                Flow::Abort(EndReason::Forfeit)
            }
        }
        Some(ClientMsg::Reveal {
            attempt_id: aid,
            secret,
        }) if phase == Phase::Reveal => {
            if aid == attempt_id {
                *slot = Some(secret);
                Flow::Continue
            } else {
                me.send(ServerMsg::Error {
                    message: "reveal for wrong attempt_id".into(),
                });
                Flow::Abort(EndReason::Forfeit)
            }
        }
        Some(_) => {
            me.send(ServerMsg::Error {
                message: "unexpected message for current phase".into(),
            });
            Flow::Abort(EndReason::Forfeit)
        }
    }
}

/// Collect both players' commit hashes (or reveal secrets), relaying chat from
/// either side concurrently while we wait.
async fn collect_phase(
    pool: &DbPool,
    match_id: Uuid,
    round_no: i32,
    attempt_id: Uuid,
    phase: Phase,
    a: &mut PlayerConn,
    b: &mut PlayerConn,
) -> PhaseOut {
    let mut va: Option<String> = None;
    let mut vb: Option<String> = None;
    while va.is_none() || vb.is_none() {
        tokio::select! {
            m = a.inbox.recv(), if va.is_none() => {
                if let Flow::Abort(reason) =
                    handle_msg(pool, match_id, round_no, attempt_id, phase, m, a, b, &mut va).await
                {
                    return PhaseOut::Abort { winner_seat: 1, reason };
                }
            }
            m = b.inbox.recv(), if vb.is_none() => {
                if let Flow::Abort(reason) =
                    handle_msg(pool, match_id, round_no, attempt_id, phase, m, b, a, &mut vb).await
                {
                    return PhaseOut::Abort { winner_seat: 0, reason };
                }
            }
        }
    }
    PhaseOut::Both(va.unwrap(), vb.unwrap())
}

/// Drive a full best-of-N match between two players, persisting as it goes.
pub async fn run_match(pool: DbPool, best_of: u32, mut a: PlayerConn, mut b: PlayerConn) {
    let match_id = Uuid::new_v4();
    let rules = rules::rules_text(best_of);
    let need = best_of / 2 + 1;

    if let Err(e) = db_ops::create_match(
        &pool,
        match_id,
        best_of as i32,
        [
            SeatInfo {
                seat: 0,
                player_id: a.player_id,
                claimed_model: a.model.clone(),
                display_name: a.display_name.clone(),
            },
            SeatInfo {
                seat: 1,
                player_id: b.player_id,
                claimed_model: b.model.clone(),
                display_name: b.display_name.clone(),
            },
        ],
    )
    .await
    {
        tracing::error!("create_match failed: {e}");
        a.send(ServerMsg::Error {
            message: "server error creating match".into(),
        });
        b.send(ServerMsg::Error {
            message: "server error creating match".into(),
        });
        return;
    }

    a.send(ServerMsg::MatchStart {
        match_id,
        opponent_model: b.model.clone(),
        best_of,
        rules: rules.clone(),
    });
    b.send(ServerMsg::MatchStart {
        match_id,
        opponent_model: a.model.clone(),
        best_of,
        rules: rules.clone(),
    });

    let mut score_a: u32 = 0;
    let mut score_b: u32 = 0;
    let mut round_no: u32 = 1;
    let mut end_reason = EndReason::WinByScore;
    let mut forced_winner: Option<u8> = None;

    'match_loop: loop {
        let mut attempt_no: u32 = 1;
        loop {
            let attempt_id = Uuid::new_v4();
            a.send(ServerMsg::RoundStart {
                match_id,
                attempt_id,
                round_no,
                attempt_no,
                score_you: score_a,
                score_them: score_b,
                rules: rules.clone(),
            });
            b.send(ServerMsg::RoundStart {
                match_id,
                attempt_id,
                round_no,
                attempt_no,
                score_you: score_b,
                score_them: score_a,
                rules: rules.clone(),
            });

            let (ha, hb) = match collect_phase(
                &pool,
                match_id,
                round_no as i32,
                attempt_id,
                Phase::Commit,
                &mut a,
                &mut b,
            )
            .await
            {
                PhaseOut::Both(ha, hb) => (ha, hb),
                PhaseOut::Abort {
                    winner_seat,
                    reason,
                } => {
                    forced_winner = Some(winner_seat);
                    end_reason = reason;
                    break 'match_loop;
                }
            };

            a.send(ServerMsg::AwaitReveal { attempt_id });
            b.send(ServerMsg::AwaitReveal { attempt_id });

            let (sa, sb) = match collect_phase(
                &pool,
                match_id,
                round_no as i32,
                attempt_id,
                Phase::Reveal,
                &mut a,
                &mut b,
            )
            .await
            {
                PhaseOut::Both(sa, sb) => (sa, sb),
                PhaseOut::Abort {
                    winner_seat,
                    reason,
                } => {
                    forced_winner = Some(winner_seat);
                    end_reason = reason;
                    break 'match_loop;
                }
            };

            // Verify each reveal against that player's own commitment.
            let ta = verify_reveal(&sa, &ha);
            let tb = verify_reveal(&sb, &hb);
            let (ta, tb): (Throw, Throw) = match (ta, tb) {
                (Some(ta), Some(tb)) => (ta, tb),
                (None, _) => {
                    a.send(ServerMsg::Error {
                        message: "reveal did not match commit".into(),
                    });
                    forced_winner = Some(1);
                    end_reason = EndReason::Forfeit;
                    break 'match_loop;
                }
                (_, None) => {
                    b.send(ServerMsg::Error {
                        message: "reveal did not match commit".into(),
                    });
                    forced_winner = Some(0);
                    end_reason = EndReason::Forfeit;
                    break 'match_loop;
                }
            };

            let outcome_a = judge(ta, tb);
            let is_tie = outcome_a == Outcome::Tie;

            if let Err(e) = db_ops::record_round(
                &pool,
                match_id,
                round_no as i32,
                attempt_no as i32,
                ta.as_str().into(),
                tb.as_str().into(),
                outcome_str(outcome_a).into(),
                is_tie,
            )
            .await
            {
                tracing::warn!("record_round failed: {e}");
            }

            if !is_tie {
                match outcome_a {
                    Outcome::Win => score_a += 1,
                    Outcome::Lose => score_b += 1,
                    Outcome::Tie => {}
                }
                let _ = db_ops::set_score(&pool, match_id, 0, score_a as i32).await;
                let _ = db_ops::set_score(&pool, match_id, 1, score_b as i32).await;
            }

            a.send(ServerMsg::RoundResult {
                attempt_id,
                round_no,
                attempt_no,
                your_throw: ta,
                their_throw: tb,
                outcome: outcome_a,
                score_you: score_a,
                score_them: score_b,
            });
            b.send(ServerMsg::RoundResult {
                attempt_id,
                round_no,
                attempt_no,
                your_throw: tb,
                their_throw: ta,
                outcome: invert(outcome_a),
                score_you: score_b,
                score_them: score_a,
            });

            if is_tie {
                attempt_no += 1;
                continue;
            }
            break;
        }

        round_no += 1;
        if score_a >= need || score_b >= need {
            // end_reason stays WinByScore (its initial value)
            break;
        }
    }

    let winner_model = match forced_winner {
        Some(0) => Some(a.model.clone()),
        Some(_) => Some(b.model.clone()),
        None => match score_a.cmp(&score_b) {
            std::cmp::Ordering::Greater => Some(a.model.clone()),
            std::cmp::Ordering::Less => Some(b.model.clone()),
            std::cmp::Ordering::Equal => None,
        },
    };

    if let Err(e) = db_ops::finalize_match(
        &pool,
        match_id,
        winner_model.clone(),
        end_reason_str(end_reason).into(),
    )
    .await
    {
        tracing::warn!("finalize_match failed: {e}");
    }

    a.send(ServerMsg::MatchEnd {
        winner_model: winner_model.clone(),
        score_you: score_a,
        score_them: score_b,
        reason: end_reason,
    });
    b.send(ServerMsg::MatchEnd {
        winner_model,
        score_you: score_b,
        score_them: score_a,
        reason: end_reason,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_forces_odd_and_clamps() {
        assert_eq!(sanitize_best_of(0), 1);
        assert_eq!(sanitize_best_of(2), 3);
        assert_eq!(sanitize_best_of(3), 3);
        assert_eq!(sanitize_best_of(1000), 101);
    }

    #[test]
    fn invert_is_correct() {
        assert_eq!(invert(Outcome::Win), Outcome::Lose);
        assert_eq!(invert(Outcome::Tie), Outcome::Tie);
    }
}
