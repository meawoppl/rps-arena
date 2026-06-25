use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::Deserialize;
use shared::{
    ChatRecord, EndReason, LeaderboardRow, MatchDetail, MatchSummary, Outcome, RoundRecord, Throw,
};
use uuid::Uuid;

use crate::AppState;

const DEFAULT_MATCH_LIMIT: i64 = 25;
const MAX_MATCH_LIMIT: i64 = 100;
const ELO_SEED: f64 = 1200.0;
const ELO_K: f64 = 24.0;

#[derive(Debug, Deserialize)]
pub struct MatchesQuery {
    limit: Option<i64>,
}

#[derive(Debug, Clone, Queryable)]
struct MatchRow {
    id: Uuid,
    best_of: i32,
    winner_model: Option<String>,
    end_reason: Option<String>,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable)]
struct ParticipantRow {
    match_id: Uuid,
    seat: i16,
    claimed_model: String,
    score: i32,
}

#[derive(Debug, Clone, Queryable)]
struct RoundAttemptRow {
    id: Uuid,
    match_id: Uuid,
    round_no: i32,
    attempt_no: i32,
    throw_a: String,
    throw_b: String,
    outcome_a: String,
}

#[derive(Debug, Clone, Queryable)]
struct ChatMessageRow {
    round_no: Option<i32>,
    from_model: String,
    text: String,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Default, Clone)]
struct ModelStats {
    matches: u32,
    match_wins: u32,
    match_losses: u32,
    match_draws: u32,
    rounds: u32,
    round_wins: u32,
    round_losses: u32,
    round_ties: u32,
    throw_dist: [u32; 3],
}

pub async fn leaderboard(
    State(app_state): State<Arc<AppState>>,
) -> Result<Json<Vec<LeaderboardRow>>, StatusCode> {
    let pool = app_state.db_pool.clone();
    let rows = tokio::task::spawn_blocking(move || {
        let mut conn = pool
            .get()
            .map_err(|err| anyhow::anyhow!("db pool error: {err}"))?;
        load_leaderboard(&mut conn)
    })
    .await
    .map_err(|err| {
        tracing::error!("leaderboard task failed: {err}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?
    .map_err(|err| {
        tracing::error!("leaderboard query failed: {err}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(rows))
}

pub async fn list_matches(
    State(app_state): State<Arc<AppState>>,
    Query(query): Query<MatchesQuery>,
) -> Result<Json<Vec<MatchSummary>>, StatusCode> {
    let limit = query
        .limit
        .unwrap_or(DEFAULT_MATCH_LIMIT)
        .clamp(1, MAX_MATCH_LIMIT);
    let pool = app_state.db_pool.clone();
    let rows = tokio::task::spawn_blocking(move || {
        let mut conn = pool
            .get()
            .map_err(|err| anyhow::anyhow!("db pool error: {err}"))?;
        load_match_summaries(&mut conn, limit)
    })
    .await
    .map_err(|err| {
        tracing::error!("list matches task failed: {err}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?
    .map_err(|err| {
        tracing::error!("list matches query failed: {err}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(rows))
}

pub async fn get_match(
    State(app_state): State<Arc<AppState>>,
    Path(match_id): Path<Uuid>,
) -> Result<Json<MatchDetail>, StatusCode> {
    let pool = app_state.db_pool.clone();
    let detail = tokio::task::spawn_blocking(move || {
        let mut conn = pool
            .get()
            .map_err(|err| ReadError::InvalidData(format!("db pool error: {err}")))?;
        load_match_detail(&mut conn, match_id)
    })
    .await
    .map_err(|err| {
        tracing::error!("match detail task failed: {err}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?
    .map_err(|err| match err {
        ReadError::NotFound => StatusCode::NOT_FOUND,
        ReadError::Db(err) => {
            tracing::error!("match detail query failed: {err}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
        ReadError::InvalidData(msg) => {
            tracing::error!("match detail failed: {msg}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    })?;

    Ok(Json(detail))
}

#[derive(Debug)]
enum ReadError {
    NotFound,
    Db(diesel::result::Error),
    InvalidData(String),
}

impl From<diesel::result::Error> for ReadError {
    fn from(err: diesel::result::Error) -> Self {
        if matches!(err, diesel::result::Error::NotFound) {
            ReadError::NotFound
        } else {
            ReadError::Db(err)
        }
    }
}

fn load_leaderboard(conn: &mut PgConnection) -> Result<Vec<LeaderboardRow>, anyhow::Error> {
    let matches = load_finished_matches(conn)?;
    let match_ids: Vec<Uuid> = matches.iter().map(|m| m.id).collect();
    let participants = load_participants(conn, &match_ids)?;
    let rounds = load_rounds(conn, &match_ids)?;

    let mut participants_by_match: HashMap<Uuid, Vec<ParticipantRow>> = HashMap::new();
    for participant in participants {
        participants_by_match
            .entry(participant.match_id)
            .or_default()
            .push(participant);
    }

    let mut rounds_by_match: HashMap<Uuid, Vec<RoundAttemptRow>> = HashMap::new();
    for round in rounds {
        rounds_by_match
            .entry(round.match_id)
            .or_default()
            .push(round);
    }

    let mut stats = BTreeMap::<String, ModelStats>::new();
    let mut ratings = BTreeMap::<String, f64>::new();

    for match_row in &matches {
        let Some(seats) = seats_for(&participants_by_match, match_row.id) else {
            continue;
        };
        let model_a = seats[0].claimed_model.clone();
        let model_b = seats[1].claimed_model.clone();

        for model in [&model_a, &model_b] {
            stats.entry(model.clone()).or_default().matches += 1;
            ratings.entry(model.clone()).or_insert(ELO_SEED);
        }

        match match_row.winner_model.as_deref() {
            Some(winner) if winner == model_a => {
                stats.entry(model_a.clone()).or_default().match_wins += 1;
                stats.entry(model_b.clone()).or_default().match_losses += 1;
            }
            Some(winner) if winner == model_b => {
                stats.entry(model_b.clone()).or_default().match_wins += 1;
                stats.entry(model_a.clone()).or_default().match_losses += 1;
            }
            _ => {
                stats.entry(model_a.clone()).or_default().match_draws += 1;
                stats.entry(model_b.clone()).or_default().match_draws += 1;
            }
        }

        if match_row.end_reason.as_deref() == Some("win_by_score") {
            update_elo(
                &mut ratings,
                &model_a,
                &model_b,
                match_row.winner_model.as_deref(),
            );
        }

        if let Some(rounds) = rounds_by_match.get(&match_row.id) {
            for round in rounds {
                accumulate_round_stats(&mut stats, &model_a, &model_b, round)?;
            }
        }
    }

    let mut rows: Vec<_> = stats
        .into_iter()
        .map(|(model, s)| {
            let decisive_rounds = s.round_wins + s.round_losses;
            LeaderboardRow {
                model: model.clone(),
                matches: s.matches,
                match_wins: s.match_wins,
                match_losses: s.match_losses,
                match_draws: s.match_draws,
                match_win_rate: rate(s.match_wins, s.matches),
                rounds: s.rounds,
                round_wins: s.round_wins,
                round_losses: s.round_losses,
                round_ties: s.round_ties,
                round_win_rate: rate(s.round_wins, decisive_rounds),
                elo: ratings.get(&model).copied().unwrap_or(ELO_SEED),
                throw_dist: s.throw_dist,
            }
        })
        .collect();

    rows.sort_by(|a, b| {
        b.elo
            .partial_cmp(&a.elo)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.match_wins.cmp(&a.match_wins))
            .then_with(|| a.model.cmp(&b.model))
    });

    Ok(rows)
}

fn load_match_summaries(
    conn: &mut PgConnection,
    limit: i64,
) -> Result<Vec<MatchSummary>, anyhow::Error> {
    let match_rows = load_recent_matches(conn, limit)?;
    let match_ids: Vec<Uuid> = match_rows.iter().map(|m| m.id).collect();
    let participants = load_participants(conn, &match_ids)?;
    let participants_by_match = group_participants(participants);

    let mut summaries = Vec::with_capacity(match_rows.len());
    for match_row in match_rows {
        if let Some(seats) = seats_for(&participants_by_match, match_row.id) {
            summaries.push(to_summary(&match_row, &seats).map_err(anyhow::Error::msg)?);
        }
    }
    Ok(summaries)
}

fn load_match_detail(conn: &mut PgConnection, match_id: Uuid) -> Result<MatchDetail, ReadError> {
    let match_row = load_match_by_id(conn, match_id)?;
    let participants = load_participants(conn, &[match_id]).map_err(ReadError::Db)?;
    let participants_by_match = group_participants(participants);
    let seats = seats_for(&participants_by_match, match_id).ok_or_else(|| {
        ReadError::InvalidData(format!("match {match_id} does not have two participants"))
    })?;
    let summary = to_summary(&match_row, &seats).map_err(ReadError::InvalidData)?;

    let rounds = load_rounds(conn, &[match_id])
        .map_err(ReadError::Db)?
        .into_iter()
        .map(to_round_record)
        .collect::<Result<Vec<_>, _>>()
        .map_err(ReadError::InvalidData)?;

    let chat = load_chat(conn, match_id)
        .map_err(ReadError::Db)?
        .into_iter()
        .map(|row| ChatRecord {
            from_model: row.from_model,
            round_no: row.round_no.map(|n| n as u32),
            text: row.text,
            created_at: row.created_at,
        })
        .collect();

    Ok(MatchDetail {
        summary,
        rounds,
        chat,
    })
}

fn load_finished_matches(conn: &mut PgConnection) -> QueryResult<Vec<MatchRow>> {
    use crate::schema::matches;
    matches::table
        .select((
            matches::id,
            matches::best_of,
            matches::winner_model,
            matches::end_reason,
            matches::started_at,
            matches::ended_at,
        ))
        .filter(matches::status.eq("finished"))
        .filter(matches::ended_at.is_not_null())
        .order(matches::ended_at.asc())
        .load(conn)
}

fn load_recent_matches(conn: &mut PgConnection, limit: i64) -> QueryResult<Vec<MatchRow>> {
    use crate::schema::matches;
    matches::table
        .select((
            matches::id,
            matches::best_of,
            matches::winner_model,
            matches::end_reason,
            matches::started_at,
            matches::ended_at,
        ))
        .filter(matches::status.eq("finished"))
        .filter(matches::ended_at.is_not_null())
        .order(matches::ended_at.desc())
        .limit(limit)
        .load(conn)
}

fn load_match_by_id(conn: &mut PgConnection, match_id: Uuid) -> Result<MatchRow, ReadError> {
    use crate::schema::matches;
    matches::table
        .select((
            matches::id,
            matches::best_of,
            matches::winner_model,
            matches::end_reason,
            matches::started_at,
            matches::ended_at,
        ))
        .find(match_id)
        .first(conn)
        .map_err(ReadError::from)
}

fn load_participants(
    conn: &mut PgConnection,
    match_ids: &[Uuid],
) -> QueryResult<Vec<ParticipantRow>> {
    use crate::schema::match_participants;
    if match_ids.is_empty() {
        return Ok(Vec::new());
    }
    match_participants::table
        .select((
            match_participants::match_id,
            match_participants::seat,
            match_participants::claimed_model,
            match_participants::score,
        ))
        .filter(match_participants::match_id.eq_any(match_ids))
        .order((
            match_participants::match_id.asc(),
            match_participants::seat.asc(),
        ))
        .load(conn)
}

fn load_rounds(conn: &mut PgConnection, match_ids: &[Uuid]) -> QueryResult<Vec<RoundAttemptRow>> {
    use crate::schema::round_attempts;
    if match_ids.is_empty() {
        return Ok(Vec::new());
    }
    round_attempts::table
        .select((
            round_attempts::id,
            round_attempts::match_id,
            round_attempts::round_no,
            round_attempts::attempt_no,
            round_attempts::throw_a,
            round_attempts::throw_b,
            round_attempts::outcome_a,
        ))
        .filter(round_attempts::match_id.eq_any(match_ids))
        .order((
            round_attempts::match_id.asc(),
            round_attempts::round_no.asc(),
            round_attempts::attempt_no.asc(),
            round_attempts::created_at.asc(),
        ))
        .load(conn)
}

fn load_chat(conn: &mut PgConnection, match_id: Uuid) -> QueryResult<Vec<ChatMessageRow>> {
    use crate::schema::chat_messages;
    chat_messages::table
        .select((
            chat_messages::round_no,
            chat_messages::from_model,
            chat_messages::text,
            chat_messages::created_at,
        ))
        .filter(chat_messages::match_id.eq(match_id))
        .order(chat_messages::created_at.asc())
        .load(conn)
}

fn group_participants(rows: Vec<ParticipantRow>) -> HashMap<Uuid, Vec<ParticipantRow>> {
    let mut grouped = HashMap::<Uuid, Vec<ParticipantRow>>::new();
    for row in rows {
        grouped.entry(row.match_id).or_default().push(row);
    }
    grouped
}

fn seats_for(
    grouped: &HashMap<Uuid, Vec<ParticipantRow>>,
    match_id: Uuid,
) -> Option<[ParticipantRow; 2]> {
    let rows = grouped.get(&match_id)?;
    let a = rows.iter().find(|row| row.seat == 0)?.clone();
    let b = rows.iter().find(|row| row.seat == 1)?.clone();
    Some([a, b])
}

fn to_summary(match_row: &MatchRow, seats: &[ParticipantRow; 2]) -> Result<MatchSummary, String> {
    Ok(MatchSummary {
        match_id: match_row.id,
        best_of: match_row.best_of as u32,
        model_a: seats[0].claimed_model.clone(),
        model_b: seats[1].claimed_model.clone(),
        score_a: seats[0].score as u32,
        score_b: seats[1].score as u32,
        winner_model: match_row.winner_model.clone(),
        reason: match_row
            .end_reason
            .as_deref()
            .map(parse_end_reason)
            .transpose()?,
        started_at: match_row.started_at,
        ended_at: match_row.ended_at,
    })
}

fn to_round_record(row: RoundAttemptRow) -> Result<RoundRecord, String> {
    Ok(RoundRecord {
        attempt_id: row.id,
        round_no: row.round_no as u32,
        attempt_no: row.attempt_no as u32,
        throw_a: parse_throw(&row.throw_a)?,
        throw_b: parse_throw(&row.throw_b)?,
        outcome_a: parse_outcome(&row.outcome_a)?,
    })
}

fn accumulate_round_stats(
    stats: &mut BTreeMap<String, ModelStats>,
    model_a: &str,
    model_b: &str,
    round: &RoundAttemptRow,
) -> Result<(), anyhow::Error> {
    let throw_a = parse_throw(&round.throw_a).map_err(anyhow::Error::msg)?;
    let throw_b = parse_throw(&round.throw_b).map_err(anyhow::Error::msg)?;
    let outcome_a = parse_outcome(&round.outcome_a).map_err(anyhow::Error::msg)?;

    let a = stats.entry(model_a.to_string()).or_default();
    a.rounds += 1;
    a.throw_dist[throw_index(throw_a)] += 1;
    match outcome_a {
        Outcome::Win => a.round_wins += 1,
        Outcome::Lose => a.round_losses += 1,
        Outcome::Tie => a.round_ties += 1,
    }

    let b = stats.entry(model_b.to_string()).or_default();
    b.rounds += 1;
    b.throw_dist[throw_index(throw_b)] += 1;
    match outcome_a {
        Outcome::Win => b.round_losses += 1,
        Outcome::Lose => b.round_wins += 1,
        Outcome::Tie => b.round_ties += 1,
    }

    Ok(())
}

fn update_elo(
    ratings: &mut BTreeMap<String, f64>,
    model_a: &str,
    model_b: &str,
    winner: Option<&str>,
) {
    let rating_a = ratings.get(model_a).copied().unwrap_or(ELO_SEED);
    let rating_b = ratings.get(model_b).copied().unwrap_or(ELO_SEED);
    let expected_a = 1.0 / (1.0 + 10_f64.powf((rating_b - rating_a) / 400.0));
    let expected_b = 1.0 - expected_a;

    let score_a = match winner {
        Some(winner) if winner == model_a => 1.0,
        Some(winner) if winner == model_b => 0.0,
        _ => 0.5,
    };
    let score_b = 1.0 - score_a;

    ratings.insert(
        model_a.to_string(),
        rating_a + ELO_K * (score_a - expected_a),
    );
    ratings.insert(
        model_b.to_string(),
        rating_b + ELO_K * (score_b - expected_b),
    );
}

fn rate(numerator: u32, denominator: u32) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn parse_throw(value: &str) -> Result<Throw, String> {
    Throw::parse(value).ok_or_else(|| format!("invalid throw: {value}"))
}

fn parse_outcome(value: &str) -> Result<Outcome, String> {
    match value {
        "win" => Ok(Outcome::Win),
        "lose" => Ok(Outcome::Lose),
        "tie" => Ok(Outcome::Tie),
        _ => Err(format!("invalid outcome: {value}")),
    }
}

fn parse_end_reason(value: &str) -> Result<EndReason, String> {
    match value {
        "win_by_score" => Ok(EndReason::WinByScore),
        "forfeit" => Ok(EndReason::Forfeit),
        "disconnect" => Ok(EndReason::Disconnect),
        "timeout" => Ok(EndReason::Timeout),
        "server_abort" => Ok(EndReason::ServerAbort),
        _ => Err(format!("invalid end reason: {value}")),
    }
}

fn throw_index(throw: Throw) -> usize {
    match throw {
        Throw::Rock => 0,
        Throw::Paper => 1,
        Throw::Scissors => 2,
    }
}
