-- 事项与来源、原文哈希、幂等请求共同持久化。
INSERT INTO followups (
    id, owner, conversation_id, kind, topic, due_at, expires_at, timezone,
    origin_key, source_run_id, source_seq, memory_ids, memory_versions, request_hash
)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
