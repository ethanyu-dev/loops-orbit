-- 用户发送授权由 OAuth 实际返回范围确认，旧连接需重新授权。
ALTER TABLE communication_connections ADD COLUMN send_authorized BOOLEAN NOT NULL DEFAULT false;
-- 开关默认关闭；每次修改推进版本和时间边界，旧任务不得采用新策略发送。
CREATE TABLE communication_takeover_settings (
    owner TEXT PRIMARY KEY REFERENCES communication_connections(owner) ON DELETE CASCADE,
    enabled BOOLEAN NOT NULL DEFAULT false,
    -- 问题以独立文件为准，此处仅存最后观察到的快照。
    topics JSONB NOT NULL DEFAULT '[]',
    threshold DOUBLE PRECISION NOT NULL DEFAULT 0.9 CHECK(threshold >= 0.5 AND threshold <= 1),
    version BIGINT NOT NULL DEFAULT 1,
    since_ms BIGINT NOT NULL DEFAULT (extract(epoch FROM clock_timestamp())*1000)::bigint
);
-- 一条源消息只有一次接管机会；不确定的发送结果不自动重发。
CREATE TABLE communication_takeover_jobs (
    id UUID PRIMARY KEY,
    source_id UUID NOT NULL REFERENCES communication_sources(id) ON DELETE CASCADE,
    message_id TEXT NOT NULL,
    source_version BIGINT NOT NULL,
    connection_version BIGINT NOT NULL,
    connection_generation UUID NOT NULL,
    settings_version BIGINT NOT NULL,
    message JSONB NOT NULL,
    status TEXT NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','evaluating','dispatching','sent','ignored','failed','unknown')),
    reason TEXT,
    topic TEXT,
    probability DOUBLE PRECISION,
    answer TEXT,
    sent_message_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(source_id,message_id)
);
CREATE INDEX communication_takeover_pending ON communication_takeover_jobs(created_at) WHERE status='queued';
