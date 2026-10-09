-- 删除意图先持久化；文件清理在后台执行，HTTP 中断或服务重启不会丢失范围。
ALTER TABLE communication_sources ADD COLUMN removal_pending BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE communication_sources ADD CONSTRAINT communication_removal_disabled CHECK(NOT removal_pending OR (NOT subscribed AND NOT enabled));
CREATE TABLE communication_removal_batches (
    id UUID PRIMARY KEY,
    owner TEXT NOT NULL REFERENCES communication_connections(owner) ON DELETE CASCADE,
    prepared BOOLEAN NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE communication_removal_items (
    batch_id UUID NOT NULL REFERENCES communication_removal_batches(id) ON DELETE CASCADE,
    -- 不引用来源外键：成功删除来源后仍保留任务结果供页面查询。
    source_id UUID NOT NULL,
    source_version BIGINT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','complete','failed')),
    error TEXT,
    PRIMARY KEY(batch_id,source_id)
);
CREATE UNIQUE INDEX communication_removal_source_pending ON communication_removal_items(source_id) WHERE status='pending';
