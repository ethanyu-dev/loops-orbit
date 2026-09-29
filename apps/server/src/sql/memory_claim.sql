-- 抽取围栏使用尝试次数；崩溃后租约到期可重试。
UPDATE memory_jobs
SET attempts = attempts + 1, available_at = now() + interval '180 seconds'
WHERE run_id = (
    SELECT run_id FROM memory_jobs
    WHERE status = 'queued' AND available_at <= now()
    ORDER BY source_seq
    FOR UPDATE SKIP LOCKED LIMIT 1
)
RETURNING run_id, owner, source_seq, attempts
