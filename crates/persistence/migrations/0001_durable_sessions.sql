CREATE TABLE agent_conversations (
    id TEXT PRIMARY KEY,
    revision BIGINT NOT NULL CHECK (revision >= 0),
    active_run_id TEXT,
    body TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE TABLE agent_runs (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES agent_conversations(id),
    status TEXT NOT NULL,
    version BIGINT NOT NULL,
    generation BIGINT NOT NULL,
    dispatch BIGINT NOT NULL,
    lease_until TIMESTAMPTZ,
    next_sequence BIGINT NOT NULL DEFAULT 0,
    event_bytes BIGINT NOT NULL DEFAULT 0,
    body TEXT NOT NULL
);
CREATE UNIQUE INDEX agent_one_active_run ON agent_runs(conversation_id)
    WHERE status NOT IN ('finished','step-limit','failed','cancelled','superseded');
CREATE INDEX agent_recovery_runs ON agent_runs(status, lease_until);
CREATE TABLE agent_messages (
    conversation_id TEXT NOT NULL REFERENCES agent_conversations(id),
    id TEXT NOT NULL,
    position BIGINT NOT NULL,
    run_id TEXT REFERENCES agent_runs(id),
    body TEXT NOT NULL,
    PRIMARY KEY (conversation_id, id),
    UNIQUE (conversation_id, position)
);
CREATE TABLE agent_attempts (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES agent_runs(id),
    generation BIGINT NOT NULL,
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    ended_at TIMESTAMPTZ,
    body TEXT NOT NULL,
    UNIQUE (run_id, generation)
);
CREATE TABLE agent_tool_executions (
    run_id TEXT NOT NULL REFERENCES agent_runs(id),
    call_id TEXT NOT NULL,
    body TEXT NOT NULL,
    PRIMARY KEY (run_id, call_id)
);
CREATE TABLE agent_events (
    run_id TEXT NOT NULL REFERENCES agent_runs(id),
    sequence BIGINT NOT NULL,
    attempt_id TEXT NOT NULL,
    step BIGINT NOT NULL,
    draft BOOLEAN NOT NULL,
    valid BOOLEAN NOT NULL DEFAULT TRUE,
    body TEXT NOT NULL,
    PRIMARY KEY (run_id, sequence)
);
CREATE TABLE agent_commands (
    scope TEXT NOT NULL,
    request_id TEXT NOT NULL,
    digest TEXT NOT NULL,
    result TEXT NOT NULL,
    PRIMARY KEY (scope, request_id)
);
CREATE TABLE agent_outbox (
    run_id TEXT NOT NULL REFERENCES agent_runs(id),
    dispatch BIGINT NOT NULL,
    published BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (run_id, dispatch)
);
