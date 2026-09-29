-- 回复投递使用独立租约，不因发送失败重新生成模型结果。
WITH candidate AS (
    SELECT id
    FROM outbox
    WHERE (status = 'queued' AND available_at <= now())
       OR (status = 'running' AND lease_until < now())
    ORDER BY created_at
    FOR UPDATE SKIP LOCKED
    LIMIT 1
)
UPDATE outbox o
SET status = 'running',
    attempts = o.attempts + 1,
    lease_token = $1,
    lease_until = now() + interval '60 seconds'
FROM candidate c
WHERE o.id = c.id
RETURNING o.id, o.reply_to, o.receiver_id, o.followup_id, o.followup_version, o.content, o.attempts
