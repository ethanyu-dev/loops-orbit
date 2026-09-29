-- 发现任务独立领取并设置持久化退避，崩溃后可恢复。
UPDATE followup_discovery
SET attempts=attempts+1,available_at=now()+interval '150 seconds'
WHERE run_id=(SELECT run_id
FROM followup_discovery
WHERE status='queued'
AND available_at<=now()
ORDER BY available_at
FOR UPDATE SKIP LOCKED LIMIT 1)
RETURNING run_id,owner,attempts
