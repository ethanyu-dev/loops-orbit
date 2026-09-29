-- 原地复用过期桶，插入和递增在同一语句内完成，跨实例共享限流状态。
INSERT INTO rate_limits (bucket, hits, expires_at)
VALUES ($1, 1, now() + interval '1 minute')
ON CONFLICT (bucket) DO UPDATE
SET hits = CASE
        WHEN rate_limits.expires_at <= now() THEN 1
        ELSE rate_limits.hits + 1
    END,
    expires_at = CASE
        WHEN rate_limits.expires_at <= now() THEN now() + interval '1 minute'
        ELSE rate_limits.expires_at
    END
RETURNING hits
