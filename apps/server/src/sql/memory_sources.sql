-- 同批补充只抽取用户原文；失败、取消、其他会话和未来输入均排除。
SELECT r.input
FROM runs r
JOIN runs source ON r.batch_id = source.batch_id AND r.conversation_id = source.conversation_id
WHERE source.id = $1 AND source.status = 'completed'
  AND r.status IN ('completed', 'superseded')
  AND r.seq <= source.seq AND r.seq > $2
ORDER BY r.seq
