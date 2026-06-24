//! Async wrappers over the (blocking) Diesel pool for the match engine's write
//! path. Each call hops onto `spawn_blocking` so the async runtime isn't stalled.

use anyhow::Result;
use chrono::Utc;
use diesel::prelude::*;
use uuid::Uuid;

use crate::db::DbPool;
use crate::models::{NewChatMessage, NewMatch, NewParticipant, NewPlayer, NewRoundAttempt};

/// One match participant's identity at match-creation time.
pub struct SeatInfo {
    pub seat: i16,
    pub player_id: Uuid,
    pub claimed_model: String,
    pub display_name: String,
}

pub async fn create_player(
    pool: &DbPool,
    id: Uuid,
    model: String,
    display_name: String,
    is_test: bool,
) -> Result<()> {
    let pool = pool.clone();
    tokio::task::spawn_blocking(move || {
        use crate::schema::players;
        let mut conn = pool.get()?;
        diesel::insert_into(players::table)
            .values(NewPlayer {
                id,
                model,
                display_name,
                is_test,
            })
            .execute(&mut conn)?;
        Ok(())
    })
    .await?
}

pub async fn create_match(
    pool: &DbPool,
    match_id: Uuid,
    best_of: i32,
    is_test: bool,
    seats: [SeatInfo; 2],
) -> Result<()> {
    let pool = pool.clone();
    tokio::task::spawn_blocking(move || {
        use crate::schema::{match_participants, matches};
        let mut conn = pool.get()?;
        conn.transaction::<_, anyhow::Error, _>(|conn| {
            diesel::insert_into(matches::table)
                .values(NewMatch {
                    id: match_id,
                    best_of,
                    status: "in_progress".to_string(),
                    is_test,
                })
                .execute(conn)?;
            for s in seats {
                diesel::insert_into(match_participants::table)
                    .values(NewParticipant {
                        id: Uuid::new_v4(),
                        match_id,
                        seat: s.seat,
                        player_id: s.player_id,
                        claimed_model: s.claimed_model,
                        display_name: s.display_name,
                    })
                    .execute(conn)?;
            }
            Ok(())
        })?;
        Ok(())
    })
    .await?
}

#[allow(clippy::too_many_arguments)]
pub async fn record_round(
    pool: &DbPool,
    match_id: Uuid,
    round_no: i32,
    attempt_no: i32,
    throw_a: String,
    throw_b: String,
    outcome_a: String,
    is_tie: bool,
) -> Result<()> {
    let pool = pool.clone();
    tokio::task::spawn_blocking(move || {
        use crate::schema::round_attempts;
        let mut conn = pool.get()?;
        diesel::insert_into(round_attempts::table)
            .values(NewRoundAttempt {
                id: Uuid::new_v4(),
                match_id,
                round_no,
                attempt_no,
                throw_a,
                throw_b,
                outcome_a,
                is_tie,
            })
            .execute(&mut conn)?;
        Ok(())
    })
    .await?
}

pub async fn record_chat(
    pool: &DbPool,
    match_id: Uuid,
    round_no: Option<i32>,
    from_model: String,
    text: String,
) -> Result<()> {
    let pool = pool.clone();
    tokio::task::spawn_blocking(move || {
        use crate::schema::chat_messages;
        let mut conn = pool.get()?;
        diesel::insert_into(chat_messages::table)
            .values(NewChatMessage {
                id: Uuid::new_v4(),
                match_id,
                round_no,
                from_model,
                text,
            })
            .execute(&mut conn)?;
        Ok(())
    })
    .await?
}

pub async fn set_score(pool: &DbPool, match_id: Uuid, seat: i16, score: i32) -> Result<()> {
    let pool = pool.clone();
    tokio::task::spawn_blocking(move || {
        use crate::schema::match_participants::dsl as p;
        let mut conn = pool.get()?;
        diesel::update(
            p::match_participants.filter(p::match_id.eq(match_id).and(p::seat.eq(seat))),
        )
        .set(p::score.eq(score))
        .execute(&mut conn)?;
        Ok(())
    })
    .await?
}

pub async fn finalize_match(
    pool: &DbPool,
    match_id: Uuid,
    winner_model: Option<String>,
    end_reason: String,
) -> Result<()> {
    let pool = pool.clone();
    tokio::task::spawn_blocking(move || {
        use crate::schema::matches::dsl as m;
        let mut conn = pool.get()?;
        diesel::update(m::matches.filter(m::id.eq(match_id)))
            .set((
                m::status.eq("finished"),
                m::winner_model.eq(winner_model),
                m::end_reason.eq(Some(end_reason)),
                m::ended_at.eq(Some(Utc::now())),
            ))
            .execute(&mut conn)?;
        Ok(())
    })
    .await?
}
