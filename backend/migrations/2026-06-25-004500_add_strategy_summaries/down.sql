ALTER TABLE round_attempts
    DROP COLUMN IF EXISTS strategy_summary_b,
    DROP COLUMN IF EXISTS strategy_summary_a;
