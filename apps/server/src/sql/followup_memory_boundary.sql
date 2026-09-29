-- 历史边界推进同时取消旧历史回访，不改变无记忆依赖的明确提醒。
UPDATE followups
SET status='cancelled',version=version+1,lease_token=NULL,error='memory_changed'
WHERE owner=$1
AND kind='checkin'
AND source_seq<=$2
AND status IN('scheduled','checking','queued')
RETURNING id
