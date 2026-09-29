-- 按有效租约写回发送结果，重试不改变出站 UUID。
UPDATE outbox
SET status=$3,error=$4,lease_until=NULL,available_at=now()+make_interval(secs=>$5),delivered_at=CASE WHEN $3='completed'
THEN now()
ELSE delivered_at END
WHERE id=$1
AND lease_token=$2
AND status='running'
