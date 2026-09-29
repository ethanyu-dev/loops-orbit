-- 聊天租约、发现尝试和遗忘边界共同约束工具写入。
SELECT EXISTS (
    SELECT 1 FROM runs
    WHERE id = $1
      AND (
          (status = 'running' AND lease_token = $2 AND $3::int IS NULL)
          OR (
              status = 'completed' AND $3::int IS NOT NULL
              AND EXISTS (
                  SELECT 1 FROM followup_discovery d
                  WHERE d.run_id = $1 AND d.status = 'queued' AND d.attempts = $3
              )
          )
      )
      AND seq > COALESCE((
          SELECT forgotten_through FROM memory_owners
          WHERE owner = (SELECT owner FROM conversations WHERE id = runs.conversation_id)
      ), 0)
)
