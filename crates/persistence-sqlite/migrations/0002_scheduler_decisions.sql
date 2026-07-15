ALTER TABLE occurrences ADD COLUMN deferred_until_utc INTEGER;

CREATE INDEX idx_occurrences_state_deferred_until
    ON occurrences(state, deferred_until_utc);

UPDATE schema_meta
SET value = '2', updated_at_utc = unixepoch() * 1000
WHERE key = 'schema_version';

