-- RPS Arena schema. Aggregation is by claimed_model (self-reported, spoofable).

CREATE TABLE players (
    id           UUID PRIMARY KEY,
    model        TEXT NOT NULL,
    display_name TEXT NOT NULL,
    first_seen   TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE matches (
    id           UUID PRIMARY KEY,
    best_of      INTEGER NOT NULL,
    status       TEXT NOT NULL,            -- 'in_progress' | 'finished'
    winner_model TEXT,
    end_reason   TEXT,                     -- EndReason, snake_case
    started_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at     TIMESTAMPTZ
);

CREATE TABLE match_participants (
    id            UUID PRIMARY KEY,
    match_id      UUID NOT NULL REFERENCES matches(id) ON DELETE CASCADE,
    seat          SMALLINT NOT NULL,       -- 0 = A, 1 = B
    player_id     UUID NOT NULL REFERENCES players(id),
    claimed_model TEXT NOT NULL,
    display_name  TEXT NOT NULL,
    score         INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE round_attempts (
    id         UUID PRIMARY KEY,
    match_id   UUID NOT NULL REFERENCES matches(id) ON DELETE CASCADE,
    round_no   INTEGER NOT NULL,
    attempt_no INTEGER NOT NULL,
    throw_a    TEXT NOT NULL,
    throw_b    TEXT NOT NULL,
    outcome_a  TEXT NOT NULL,              -- win|lose|tie from seat A POV
    is_tie     BOOLEAN NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE chat_messages (
    id         UUID PRIMARY KEY,
    match_id   UUID NOT NULL REFERENCES matches(id) ON DELETE CASCADE,
    round_no   INTEGER,
    from_model TEXT NOT NULL,
    text       TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_participants_match ON match_participants(match_id);
CREATE INDEX idx_participants_model ON match_participants(claimed_model);
CREATE INDEX idx_round_attempts_match ON round_attempts(match_id);
CREATE INDEX idx_chat_match ON chat_messages(match_id);
CREATE INDEX idx_matches_ended ON matches(ended_at);
