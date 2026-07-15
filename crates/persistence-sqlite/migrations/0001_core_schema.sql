CREATE TABLE schema_meta (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL,
    updated_at_utc INTEGER NOT NULL
) STRICT;

INSERT INTO schema_meta (key, value, updated_at_utc)
VALUES ('schema_version', '1', unixepoch() * 1000);

CREATE TABLE reminders (
    id TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_at_utc INTEGER NOT NULL,
    updated_at_utc INTEGER NOT NULL,
    deleted_at_utc INTEGER,
    CHECK (length(trim(id)) > 0),
    CHECK (length(trim(title)) > 0),
    CHECK (deleted_at_utc IS NULL OR deleted_at_utc >= created_at_utc)
) STRICT;

CREATE TABLE schedule_rules (
    id TEXT PRIMARY KEY NOT NULL,
    reminder_id TEXT NOT NULL UNIQUE,
    rule_type TEXT NOT NULL,
    timezone_mode TEXT NOT NULL,
    timezone_id TEXT,
    config_json TEXT NOT NULL CHECK (json_valid(config_json)),
    created_at_utc INTEGER NOT NULL,
    updated_at_utc INTEGER NOT NULL,
    FOREIGN KEY (reminder_id) REFERENCES reminders(id) ON UPDATE RESTRICT ON DELETE RESTRICT
) STRICT;

CREATE TABLE reminder_policies (
    id TEXT PRIMARY KEY NOT NULL,
    reminder_id TEXT NOT NULL UNIQUE,
    delivery_json TEXT NOT NULL CHECK (json_valid(delivery_json)),
    sound_json TEXT NOT NULL CHECK (json_valid(sound_json)),
    snooze_json TEXT NOT NULL CHECK (json_valid(snooze_json)),
    missed_json TEXT NOT NULL CHECK (json_valid(missed_json)),
    dnd_json TEXT NOT NULL CHECK (json_valid(dnd_json)),
    created_at_utc INTEGER NOT NULL,
    updated_at_utc INTEGER NOT NULL,
    FOREIGN KEY (reminder_id) REFERENCES reminders(id) ON UPDATE RESTRICT ON DELETE RESTRICT
) STRICT;

CREATE TABLE occurrences (
    id TEXT PRIMARY KEY NOT NULL,
    reminder_id TEXT NOT NULL,
    reminder_revision INTEGER NOT NULL CHECK (reminder_revision > 0),
    occurrence_key TEXT NOT NULL,
    scheduled_at_utc INTEGER NOT NULL,
    scheduled_local TEXT NOT NULL,
    timezone_id TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending',
    result TEXT,
    suppression_reason TEXT,
    display_at_utc INTEGER,
    snooze_due_at_utc INTEGER,
    snooze_count INTEGER NOT NULL DEFAULT 0 CHECK (snooze_count >= 0),
    presented_at_utc INTEGER,
    handled_at_utc INTEGER,
    merged_into_id TEXT,
    claim_token TEXT,
    claimed_at_utc INTEGER,
    created_at_utc INTEGER NOT NULL,
    updated_at_utc INTEGER NOT NULL,
    UNIQUE (reminder_id, occurrence_key),
    FOREIGN KEY (reminder_id) REFERENCES reminders(id) ON UPDATE RESTRICT ON DELETE RESTRICT,
    FOREIGN KEY (merged_into_id) REFERENCES occurrences(id) ON UPDATE RESTRICT ON DELETE RESTRICT,
    CHECK (length(trim(occurrence_key)) > 0),
    CHECK ((claim_token IS NULL) = (claimed_at_utc IS NULL))
) STRICT;

CREATE TABLE delivery_attempts (
    id TEXT PRIMARY KEY NOT NULL,
    occurrence_id TEXT NOT NULL,
    channel TEXT NOT NULL,
    status TEXT NOT NULL,
    attempted_at_utc INTEGER NOT NULL,
    completed_at_utc INTEGER,
    error_code TEXT,
    diagnostic_json TEXT CHECK (diagnostic_json IS NULL OR json_valid(diagnostic_json)),
    FOREIGN KEY (occurrence_id) REFERENCES occurrences(id) ON UPDATE RESTRICT ON DELETE RESTRICT
) STRICT;

CREATE TABLE pause_sessions (
    id TEXT PRIMARY KEY NOT NULL,
    scope TEXT NOT NULL,
    reminder_id TEXT,
    starts_at_utc INTEGER NOT NULL,
    ends_at_utc INTEGER,
    cancelled_at_utc INTEGER,
    reason TEXT,
    created_at_utc INTEGER NOT NULL,
    CHECK (
        (scope = 'global' AND reminder_id IS NULL) OR
        (scope = 'reminder' AND reminder_id IS NOT NULL)
    ),
    CHECK (ends_at_utc IS NULL OR ends_at_utc > starts_at_utc),
    FOREIGN KEY (reminder_id) REFERENCES reminders(id) ON UPDATE RESTRICT ON DELETE RESTRICT
) STRICT;

CREATE TABLE settings (
    key TEXT PRIMARY KEY NOT NULL,
    value_json TEXT NOT NULL CHECK (json_valid(value_json)),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
    updated_at_utc INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_reminders_enabled_deleted
    ON reminders(enabled, deleted_at_utc);
CREATE INDEX idx_occurrences_reminder_scheduled
    ON occurrences(reminder_id, scheduled_at_utc);
CREATE INDEX idx_occurrences_state_snooze_due
    ON occurrences(state, snooze_due_at_utc);
CREATE INDEX idx_occurrences_scheduled_result
    ON occurrences(scheduled_at_utc, result);
CREATE INDEX idx_pause_sessions_scope_ends
    ON pause_sessions(scope, ends_at_utc);
CREATE INDEX idx_delivery_attempts_occurrence_attempted
    ON delivery_attempts(occurrence_id, attempted_at_utc);

