-- 遗忘使所有仍可能使用旧背景的生成失去租约，不删除可查看历史。
UPDATE runs
SET status = 'cancelled', phase = 'cancelled', partial_content = '',
    lease_token = NULL, lease_until = NULL, finished_at = now()
WHERE conversation_id IN (SELECT id FROM conversations WHERE owner = $1)
  AND seq <= $2
  AND status IN ('queued', 'running', 'superseded')
