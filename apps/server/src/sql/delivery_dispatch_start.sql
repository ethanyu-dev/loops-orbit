-- 在会话锁内记录 HTTP 起点，使取消反馈准确反映在途投递。
UPDATE outbox
SET dispatch_started_at=now()
WHERE id=$1
AND lease_token=$2
AND status='running'
AND (followup_id IS NULL
OR EXISTS(SELECT 1
FROM followups f
WHERE f.id=outbox.followup_id
AND f.version=outbox.followup_version
AND f.status='queued'
AND f.expires_at>now()))
