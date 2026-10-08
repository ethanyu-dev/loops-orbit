DELETE
FROM linear_oauth_states o USING sessions s
WHERE o.state_hash=$1
  AND o.browser_hash=$2
  AND o.expires_at>now()
  AND s.token_hash=o.session_hash
  AND s.expires_at>now()
  AND s.grant_id IS NULL
  AND s.admin_fingerprint=$3
RETURNING o.verifier,o.version,o.session_hash;
