-- 回访仅使用有效历史，遵守当前身份的遗忘边界。
SELECT role, content FROM messages
WHERE conversation_id = $1 AND context_visible
  AND (
      run_id IS NULL
      OR run_id IN (
          SELECT id FROM runs
          WHERE status IN ('completed', 'superseded')
            AND seq > COALESCE((SELECT forgotten_through FROM memory_owners WHERE owner = $2), 0)
      )
  )
ORDER BY seq DESC LIMIT 16
