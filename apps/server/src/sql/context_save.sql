-- 摘要只能前进，失去租约的旧执行器不能提交检查点。
UPDATE conversations
SET context_summary = $2, summary_through = $3
WHERE id = $1 AND summary_through = $4
  AND EXISTS (
      SELECT 1 FROM runs
      WHERE id = $5 AND status = 'running' AND lease_token = $6
  )
