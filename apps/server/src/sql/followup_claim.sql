-- 明确提醒和回访由独立 worker 消费；模型慢请求不会挡住准时提醒。
WITH candidate AS (
    SELECT f.id FROM followups f
    WHERE f.kind=$1 AND f.due_at<=now()
      AND (f.status='scheduled' OR (f.status='checking' AND f.lease_until<now()))
      AND (f.source_run_id IS NULL OR EXISTS(SELECT 1 FROM runs r WHERE r.id=f.source_run_id AND r.status='completed'))
    ORDER BY f.due_at
    FOR UPDATE SKIP LOCKED LIMIT 1
)
UPDATE followups f SET status='checking',lease_token=$2,lease_until=now()+interval '90 seconds',attempts=attempts+1
FROM candidate c WHERE f.id=c.id
RETURNING f.id,f.owner,f.conversation_id,f.kind,f.topic,f.due_at,f.expires_at,f.timezone,f.status,f.version,f.source_run_id,f.source_seq,f.observed_seq,f.memory_ids,f.memory_versions,f.lease_token,f.attempts,f.error,f.sent_at,f.updated_at
