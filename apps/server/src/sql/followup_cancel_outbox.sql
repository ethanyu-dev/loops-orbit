-- 已开始的 HTTP 无法撤回，但取消后不再重试该版本。
UPDATE outbox
SET status='cancelled', lease_until=NULL
WHERE followup_id=$1 AND status IN ('queued','running')
