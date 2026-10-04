-- Existing folders remain roots; names are now unique among siblings.
ALTER TABLE folders
    ADD COLUMN parent_id UUID NULL,
    DROP CONSTRAINT folders_owner_type_owner_id_name_key,
    ADD CONSTRAINT folders_owner_identity_key UNIQUE (id, owner_type, owner_id),
    ADD CONSTRAINT folders_parent_owner_fkey
        FOREIGN KEY (parent_id, owner_type, owner_id)
        REFERENCES folders (id, owner_type, owner_id) ON DELETE RESTRICT,
    ADD CONSTRAINT folders_not_own_parent CHECK (parent_id IS DISTINCT FROM id);

CREATE UNIQUE INDEX folders_root_name_key
    ON folders (owner_type, owner_id, name) WHERE parent_id IS NULL;
CREATE UNIQUE INDEX folders_sibling_name_key
    ON folders (owner_type, owner_id, parent_id, name) WHERE parent_id IS NOT NULL;
CREATE INDEX folders_parent_idx ON folders (parent_id);

-- Support root/direct-child pagination without changing legacy all-files queries.
CREATE INDEX files_owner_folder_created_idx
    ON files (owner_type, owner_id, folder_id, created_at DESC, id DESC);
