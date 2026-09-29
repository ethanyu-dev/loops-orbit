-- 原文修正或遗忘停止未发送事项，已发送历史仅解除上下文资格。
UPDATE followups
SET status=CASE WHEN status IN('scheduled','checking','queued')
THEN 'cancelled'
ELSE status END,version=version+1,lease_token=NULL,error='memory_changed',updated_at=now()
WHERE owner=$1
AND ($2=ANY(memory_ids)
OR source_run_id=$3)
RETURNING id
