CREATE TABLE plan_assignments (
    owner_type TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    plan TEXT NOT NULL,
    assigned_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (owner_type, owner_id)
);

COMMENT ON TABLE plan_assignments IS 'used in premium_mode=''mirror''; ignored in ''everyone''';
