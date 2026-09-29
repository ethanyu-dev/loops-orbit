-- 弃用排队正文并推进版本；暂时静默重新判断，无权限则取消。
UPDATE followups
SET status=CASE WHEN $3
THEN 'cancelled' WHEN expires_at<=now()+interval '1 hour'
THEN 'expired'
ELSE 'scheduled' END,due_at=LEAST(now()+interval '1 hour',expires_at-interval '1 millisecond'),version=version+1,attempts=0
WHERE id=$1
AND version=$2
AND status='queued'
