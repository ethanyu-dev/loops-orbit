-- 恢复时清理过期、取消和旧版本出站记录。
UPDATE outbox
SET status='cancelled',lease_until=NULL
WHERE followup_id IS NOT NULL
AND status IN('queued','running')
AND NOT EXISTS(SELECT 1
FROM followups f
WHERE f.id=outbox.followup_id
AND f.version=outbox.followup_version
AND f.status='queued')
