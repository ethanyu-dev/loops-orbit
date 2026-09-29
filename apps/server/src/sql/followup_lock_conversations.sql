-- 跨会话动作采用稳定锁顺序，避免与发送和遗忘交叉死锁。
SELECT id
FROM conversations
WHERE owner=$1
AND (id=(SELECT conversation_id
FROM followups
WHERE id=$2
AND owner=$1)
OR id=$3)
ORDER BY id FOR UPDATE
