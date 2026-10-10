-- 会话行和任务行一起领取，多个 worker 不会同时领取同一会话。
UPDATE communication_takeover_jobs SET status='evaluating',updated_at=now()
WHERE id=(
    SELECT j.id FROM communication_takeover_jobs j
    JOIN communication_takeover_turns t ON t.id=j.turn_id AND t.revision=j.turn_revision
    JOIN communication_takeover_sessions s ON s.source_id=j.source_id AND s.epoch=t.epoch
    JOIN communication_sources c ON c.id=s.source_id
    WHERE j.status='queued' AND t.status='pending' AND s.mode='auto'
      AND s.window_ready AND c.window_end IS NULL
      AND t.available_at<=now() AND (s.last_sent_at IS NULL OR s.last_sent_at<=now()-interval '5 seconds')
      AND NOT EXISTS(SELECT 1 FROM communication_takeover_jobs other WHERE other.source_id=j.source_id AND other.status IN ('evaluating','dispatching'))
    ORDER BY t.available_at,j.id FOR UPDATE OF s,j SKIP LOCKED LIMIT 1
)
RETURNING id,source_id,message,source_version,connection_version,connection_generation,settings_version,turn_id,turn_revision
