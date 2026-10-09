-- 候选与已发布知识独立于个人记忆；只有管理员明确发布的正文可以对外检索。
CREATE TABLE knowledge_entries (
    id UUID PRIMARY KEY,
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'candidate' CHECK(status IN ('candidate','published','rejected','revoked')),
    version BIGINT NOT NULL DEFAULT 1,
    extraction_day DATE,
    evidence JSONB NOT NULL DEFAULT '[]',
    fingerprint TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(extraction_day,fingerprint)
);
CREATE INDEX knowledge_published ON knowledge_entries(updated_at) WHERE status='published';
-- 每日自动扫描与手选文件分别排队；临时输入快照供断点恢复，完成后清空。
-- 没有沟通文件外键，原文后续变化不改变已经提取的知识与审核结论。
CREATE TABLE knowledge_jobs (
    id UUID PRIMARY KEY,
    kind TEXT NOT NULL CHECK(kind IN ('daily','manual')),
    -- 自动任务按日期去重；手动任务按选中文件版本的摘要去重，不保存文件引用。
    scope_key TEXT NOT NULL,
    scan_day DATE NOT NULL,
    messages JSONB,
    next_offset BIGINT NOT NULL DEFAULT 0,
    skipped_count BIGINT NOT NULL DEFAULT 0,
    created_count BIGINT NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','running','completed','failed')),
    attempts INTEGER NOT NULL DEFAULT 0,
    lease_token UUID,
    available_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    error TEXT,
    UNIQUE(kind,scope_key)
);
CREATE TABLE knowledge_state (
    singleton BOOLEAN PRIMARY KEY DEFAULT true CHECK(singleton),
    revision BIGINT NOT NULL DEFAULT 1,
    -- 安装日起按日推进，首次到点扫描昨天，重启补跑之后错过的日期。
    next_scan_day DATE NOT NULL DEFAULT ((now() AT TIME ZONE 'Asia/Shanghai')::date - 1)
);
INSERT INTO knowledge_state DEFAULT VALUES;
ALTER TABLE runs ADD COLUMN knowledge_revision BIGINT;

-- 发布内容变化时清空已消费知识的推理上下文，取消尚未完成的生成和未开始投递的旧回复。
CREATE FUNCTION invalidate_shared_knowledge() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF (TG_OP <> 'INSERT' AND OLD.status='published') OR (TG_OP <> 'DELETE' AND NEW.status='published') THEN
        UPDATE knowledge_state SET revision=revision+1;
        UPDATE conversations SET context_summary='',summary_through=GREATEST(summary_through,COALESCE((SELECT max(seq) FROM runs),0))
        WHERE id IN (SELECT conversation_id FROM runs WHERE knowledge_revision IS NOT NULL);
        UPDATE runs SET status='cancelled',phase='cancelled',partial_content='',lease_token=NULL,lease_until=NULL,finished_at=now()
        WHERE knowledge_revision IS NOT NULL AND status IN ('running','queued');
        UPDATE outbox SET status='cancelled',lease_until=NULL
        WHERE id IN (SELECT id FROM runs WHERE knowledge_revision IS NOT NULL)
        AND status IN ('queued','running') AND dispatch_started_at IS NULL;
    END IF;
    RETURN NULL;
END $$;
CREATE TRIGGER knowledge_publication_changed AFTER INSERT OR UPDATE OR DELETE ON knowledge_entries
FOR EACH ROW EXECUTE FUNCTION invalidate_shared_knowledge();
