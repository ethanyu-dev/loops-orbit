-- 跳过其他 worker 持有的行锁，且同一会话只能领取最早的未完成任务。
WITH candidate AS (
    SELECT r.id
    FROM runs r
    WHERE (
        (r.status = 'queued' AND r.available_at <= now())
        OR (r.status = 'running' AND r.lease_until < now())
    )
    AND NOT EXISTS (
        SELECT 1
        FROM runs previous
        WHERE previous.conversation_id = r.conversation_id
          AND previous.seq < r.seq
          AND previous.status IN ('queued', 'running')
    )
    ORDER BY r.seq
    FOR UPDATE OF r SKIP LOCKED
    LIMIT 1
)
UPDATE runs r
SET status = 'running',
    partial_content = '',
    phase = 'context',
    attempts = r.attempts + 1,
    lease_token = $1,
    lease_until = now() + interval '180 seconds'
FROM candidate c
WHERE r.id = c.id
RETURNING r.id, r.conversation_id, r.seq, r.batch_id, r.input, r.attempts,
          r.lease_token, r.reply_to
