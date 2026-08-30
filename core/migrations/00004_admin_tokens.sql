CREATE TABLE admin_tokens (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL UNIQUE,
    token_hash TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at TIMESTAMPTZ NULL
);

COMMENT ON TABLE admin_tokens IS 'token_hash is SHA-256 hex of the raw token; raw token never stored';
