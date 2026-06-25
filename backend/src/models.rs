//! Diesel models for the match engine (write path). Read-side Queryable models
//! for the leaderboard/transcript are added alongside the read API.

use diesel::prelude::*;
use uuid::Uuid;

use crate::schema::{chat_messages, match_participants, matches, players, round_attempts};

#[derive(Debug, Insertable)]
#[diesel(table_name = players)]
pub struct NewPlayer {
    pub id: Uuid,
    pub model: String,
    pub display_name: String,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = matches)]
pub struct NewMatch {
    pub id: Uuid,
    pub best_of: i32,
    pub status: String,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = match_participants)]
pub struct NewParticipant {
    pub id: Uuid,
    pub match_id: Uuid,
    pub seat: i16,
    pub player_id: Uuid,
    pub claimed_model: String,
    pub display_name: String,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = round_attempts)]
pub struct NewRoundAttempt {
    pub id: Uuid,
    pub match_id: Uuid,
    pub round_no: i32,
    pub attempt_no: i32,
    pub throw_a: String,
    pub throw_b: String,
    pub outcome_a: String,
    pub is_tie: bool,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = chat_messages)]
pub struct NewChatMessage {
    pub id: Uuid,
    pub match_id: Uuid,
    pub round_no: Option<i32>,
    pub from_model: String,
    pub text: String,
}
