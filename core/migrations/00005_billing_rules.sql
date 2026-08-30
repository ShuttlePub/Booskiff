CREATE TABLE billing_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_type TEXT NULL,
    owner_id TEXT NULL,
    key TEXT NOT NULL,
    value JSONB NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX billing_rules_scope_key_uq
    ON billing_rules ((COALESCE(owner_type, '')), (COALESCE(owner_id, '')), key);

COMMENT ON TABLE billing_rules IS 'NULL owner means global rule; resolution order plan default -> global -> owner-specific';
