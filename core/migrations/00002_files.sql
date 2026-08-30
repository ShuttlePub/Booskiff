CREATE TABLE files (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_type TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    folder_id UUID NULL REFERENCES folders(id) ON DELETE SET NULL,
    name TEXT NOT NULL,
    mime_type TEXT NOT NULL,
    size_bytes BIGINT NOT NULL CHECK (size_bytes >= 0),
    public_key TEXT NULL UNIQUE,
    is_public BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX files_owner_idx ON files (owner_type, owner_id);
CREATE INDEX files_folder_idx ON files (folder_id);
CREATE INDEX files_public_key_idx ON files (public_key) WHERE is_public;
