-- 只记录成功提交的分页，累计值包含重放和每日复查，不当作唯一消息数量。
ALTER TABLE communication_history_jobs ADD COLUMN pages_processed BIGINT NOT NULL DEFAULT 0;
ALTER TABLE communication_history_jobs ADD COLUMN last_progress_at TIMESTAMPTZ;
