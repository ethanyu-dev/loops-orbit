-- 同一批连续输入可以多次替换执行，用户消息保留，旧生成失去写回资格。
ALTER TABLE runs DROP CONSTRAINT runs_status_check;
ALTER TABLE runs ADD CONSTRAINT runs_status_check
    CHECK (status IN ('queued', 'running', 'completed', 'failed', 'superseded', 'cancelled'));
ALTER TABLE runs ADD COLUMN batch_id UUID;
UPDATE runs SET batch_id = id;
ALTER TABLE runs ALTER COLUMN batch_id SET NOT NULL;
ALTER TABLE runs ADD COLUMN partial_content TEXT NOT NULL DEFAULT '';
ALTER TABLE runs ADD COLUMN phase TEXT NOT NULL DEFAULT 'queued';
-- 摘要只覆盖已经稳定的历史边界，不包含当前仍在生成的答案。
ALTER TABLE conversations ADD COLUMN context_summary TEXT NOT NULL DEFAULT '';
ALTER TABLE conversations ADD COLUMN summary_through BIGINT NOT NULL DEFAULT 0;
