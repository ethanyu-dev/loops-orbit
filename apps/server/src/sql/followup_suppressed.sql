-- 冷却、在途回访与更新后的用户输入均禁止发送旧正文。
SELECT EXISTS (
    SELECT 1 FROM followup_preferences
    WHERE owner = $1 AND last_checkin_at > now() - make_interval(mins => $2)
)
OR EXISTS (
    SELECT 1 FROM followups
    WHERE owner = $1 AND kind = 'checkin' AND status = 'queued' AND id <> $3
)
OR EXISTS (
    SELECT 1 FROM runs
    WHERE conversation_id IN (SELECT id FROM conversations WHERE owner = $1)
      AND (
          created_at > now() - interval '15 minutes'
          OR status IN ('queued', 'running')
          OR ($4::bigint IS NOT NULL AND seq > $4)
      )
)
