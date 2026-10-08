INSERT INTO communication_sources(id,owner,chat_id,label,start_at,watermark,day_timezone,chat_mode)
SELECT $1,$2,$3,$4,$5,$5,'Asia/Shanghai','p2p'
WHERE NOT EXISTS(SELECT 1 FROM communication_exclusions WHERE owner=$2 AND chat_id=$3)
ON CONFLICT(owner,chat_id) DO NOTHING
