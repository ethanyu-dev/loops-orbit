-- 升级只废弃尚未发送的旧任务；未知投递记录保留，不能在重启后自动补发。
UPDATE communication_takeover_jobs SET status='ignored',reason='conversation_upgrade',updated_at=now()
WHERE status IN ('queued','evaluating');

-- 会话状态以来源为边界，代际和版本改变时重建上下文，不复用旧账号的数据。
CREATE TABLE communication_takeover_sessions (
    source_id UUID PRIMARY KEY REFERENCES communication_sources(id) ON DELETE CASCADE,
    source_version BIGINT NOT NULL,
    connection_version BIGINT NOT NULL,
    connection_generation UUID NOT NULL,
    settings_version BIGINT NOT NULL,
    epoch UUID NOT NULL,
    version BIGINT NOT NULL DEFAULT 1,
    mode TEXT NOT NULL DEFAULT 'auto' CHECK(mode IN ('auto','human','paused','uncertain')),
    boundary_ms BIGINT NOT NULL,
    last_activity_ms BIGINT NOT NULL DEFAULT 0,
    human_until_ms BIGINT,
    topic TEXT,
    last_sent_at TIMESTAMPTZ,
    window_ready BOOLEAN NOT NULL DEFAULT false,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- 已见消息独立于轮次保留，分页重放、迟到编辑不能重新回答已结束的问题。
CREATE TABLE communication_takeover_inputs (
    source_id UUID NOT NULL REFERENCES communication_takeover_sessions(source_id) ON DELETE CASCADE,
    message_id TEXT NOT NULL,
    message JSONB NOT NULL,
    PRIMARY KEY(source_id,message_id)
);
CREATE TABLE communication_takeover_turns (
    id UUID PRIMARY KEY,
    source_id UUID NOT NULL REFERENCES communication_takeover_sessions(source_id) ON DELETE CASCADE,
    epoch UUID NOT NULL,
    revision BIGINT NOT NULL DEFAULT 1,
    inputs JSONB NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','closed')),
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    available_at TIMESTAMPTZ NOT NULL DEFAULT now()+interval '3 seconds'
);
CREATE UNIQUE INDEX communication_takeover_open_turn ON communication_takeover_turns(source_id) WHERE status='pending';
ALTER TABLE communication_takeover_jobs DROP CONSTRAINT communication_takeover_jobs_source_id_message_id_key;
ALTER TABLE communication_takeover_jobs ADD COLUMN turn_id UUID REFERENCES communication_takeover_turns(id) ON DELETE CASCADE;
ALTER TABLE communication_takeover_jobs ADD COLUMN turn_revision BIGINT;
ALTER TABLE communication_takeover_jobs ADD COLUMN context JSONB;
ALTER TABLE communication_takeover_jobs ADD COLUMN reply_kind TEXT;
CREATE UNIQUE INDEX communication_takeover_turn_job ON communication_takeover_jobs(turn_id,turn_revision);
CREATE UNIQUE INDEX communication_takeover_legacy_job ON communication_takeover_jobs(source_id,message_id) WHERE turn_id IS NULL;
