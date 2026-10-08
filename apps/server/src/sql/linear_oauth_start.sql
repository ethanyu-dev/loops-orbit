INSERT INTO linear_oauth_states(state_hash,browser_hash,session_hash,verifier,version,expires_at)
SELECT $1,$2,$3,$4,version,now()+interval '10 minutes'
FROM linear_guard
WHERE id=true;
