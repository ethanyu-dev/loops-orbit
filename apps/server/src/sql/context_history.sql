-- 摘要边界之后、当前输入之前的稳定原文；取消和失败的生成不作为历史答案。
SELECT r.seq, r.batch_id, r.input, m.content AS answer
FROM runs r
LEFT JOIN messages m ON m.run_id = r.id AND m.role = 'assistant' AND r.status = 'completed'
WHERE r.conversation_id = $1 AND r.seq > $2 AND r.seq < $3
  AND r.status IN ('completed', 'superseded')
ORDER BY r.seq
LIMIT $4
