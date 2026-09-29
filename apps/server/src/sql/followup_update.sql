-- 改期或结束推进版本，使旧判断与出站队列失效。
UPDATE followups
SET status=$3,topic=COALESCE($4,topic),due_at=COALESCE($5,due_at),expires_at=CASE WHEN $5::timestamptz IS NOT NULL
THEN $5+interval '1 day'
ELSE expires_at END,timezone=$6,version=version+1,lease_token=NULL,lease_until=NULL,attempts=0,error=NULL,updated_at=now()
WHERE id=$1
AND owner=$2
