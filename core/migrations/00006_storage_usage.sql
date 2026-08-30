CREATE TABLE storage_usage (
    owner_type TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    used_bytes BIGINT NOT NULL DEFAULT 0 CHECK (used_bytes >= 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (owner_type, owner_id)
);

COMMENT ON TABLE storage_usage IS 'metering = received bytes only; decrement uses GREATEST(used_bytes - delta, 0)';
