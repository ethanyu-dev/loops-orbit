INSERT INTO linear_connections (
    owner, generation, user_id, user_name, workspace_id,
    workspace_name, workspace_slug, credentials, expires_at, scopes
)
VALUES ('admin', $1, $2, $3, $4, $5, $6, $7, $8, $9)
ON CONFLICT (owner) DO UPDATE SET
    generation = excluded.generation,
    user_id = excluded.user_id,
    user_name = excluded.user_name,
    workspace_id = excluded.workspace_id,
    workspace_name = excluded.workspace_name,
    workspace_slug = excluded.workspace_slug,
    credentials = excluded.credentials,
    expires_at = excluded.expires_at,
    scopes = excluded.scopes,
    status = 'active';
