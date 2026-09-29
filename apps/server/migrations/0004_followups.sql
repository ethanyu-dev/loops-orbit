-- 调度状态以数据库为准，不依赖进程内计时器；回访偏好按身份隔离。
CREATE TABLE followup_preferences (
    owner TEXT PRIMARY KEY,
    timezone TEXT NOT NULL DEFAULT 'Asia/Shanghai',
    enabled BOOLEAN NOT NULL DEFAULT false,
    quiet_start INT NOT NULL DEFAULT 1320 CHECK(quiet_start BETWEEN 0 AND 1439),
    quiet_end INT NOT NULL DEFAULT 480 CHECK(quiet_end BETWEEN 0 AND 1439),
    min_interval_minutes INT NOT NULL DEFAULT 1440 CHECK(min_interval_minutes BETWEEN 60 AND 10080),
    last_checkin_at TIMESTAMPTZ,
    version BIGINT NOT NULL DEFAULT 1
);
CREATE TABLE followups (
    id UUID PRIMARY KEY,
    owner TEXT NOT NULL REFERENCES followup_preferences(owner),
    conversation_id UUID NOT NULL REFERENCES conversations(id),
    kind TEXT NOT NULL CHECK(kind IN ('reminder','checkin')),
    topic TEXT NOT NULL,
    due_at TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    timezone TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'scheduled' CHECK(status IN ('scheduled','checking','queued','sent','completed','cancelled','expired','failed')),
    version BIGINT NOT NULL DEFAULT 1,
    origin_key TEXT NOT NULL,
    request_hash TEXT NOT NULL,
    source_run_id UUID REFERENCES runs(id),
    source_seq BIGINT NOT NULL DEFAULT 0,
    observed_seq BIGINT,
    memory_ids UUID[] NOT NULL DEFAULT '{}',
    memory_versions JSONB NOT NULL DEFAULT '{}',
    lease_token UUID,
    lease_until TIMESTAMPTZ,
    attempts INT NOT NULL DEFAULT 0,
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    sent_at TIMESTAMPTZ,
    UNIQUE(owner,origin_key),
    CHECK(expires_at > due_at)
);
CREATE INDEX followups_due ON followups(kind,due_at) WHERE status IN ('scheduled','checking');
CREATE INDEX followups_owner ON followups(owner,updated_at DESC);
-- 保存工具执行结果，模型请求重试不会重复创建或改期同一个动作。
CREATE TABLE followup_operations (
    run_id UUID NOT NULL REFERENCES runs(id),
    operation_key TEXT NOT NULL,
    result JSONB NOT NULL,
    PRIMARY KEY(run_id,operation_key)
);
-- 回访发现与准时提醒分开消费，慢模型不会挡住提醒调度。
CREATE TABLE followup_discovery (
    run_id UUID PRIMARY KEY REFERENCES runs(id),
    owner TEXT NOT NULL,
    attempts INT NOT NULL DEFAULT 0,
    available_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    status TEXT NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','completed','failed'))
);
ALTER TABLE messages ALTER COLUMN run_id DROP NOT NULL;
ALTER TABLE messages ADD COLUMN kind TEXT NOT NULL DEFAULT 'conversation' CHECK(kind IN ('conversation','followup'));
ALTER TABLE messages ADD COLUMN followup_id UUID REFERENCES followups(id);
ALTER TABLE messages ADD COLUMN followup_version BIGINT;
ALTER TABLE messages ADD COLUMN context_visible BOOLEAN NOT NULL DEFAULT true;
ALTER TABLE messages ADD COLUMN read_at TIMESTAMPTZ;
ALTER TABLE messages ADD CONSTRAINT messages_source CHECK(
    (kind='conversation' AND run_id IS NOT NULL AND followup_id IS NULL)
    OR (kind='followup' AND run_id IS NULL AND followup_id IS NOT NULL AND role='assistant')
);
CREATE UNIQUE INDEX messages_followup_once ON messages(followup_id,followup_version) WHERE kind='followup';
ALTER TABLE outbox ALTER COLUMN reply_to DROP NOT NULL;
ALTER TABLE outbox ADD COLUMN receiver_id TEXT;
ALTER TABLE outbox ADD COLUMN followup_id UUID REFERENCES followups(id);
ALTER TABLE outbox ADD COLUMN followup_version BIGINT;
ALTER TABLE outbox ADD COLUMN dispatch_started_at TIMESTAMPTZ;
ALTER TABLE outbox ADD COLUMN delivered_at TIMESTAMPTZ;
ALTER TABLE outbox ADD CONSTRAINT outbox_destination CHECK(
    (reply_to IS NOT NULL AND receiver_id IS NULL AND followup_id IS NULL)
    OR (reply_to IS NULL AND receiver_id IS NOT NULL AND followup_id IS NOT NULL)
);
CREATE UNIQUE INDEX outbox_followup_once ON outbox(followup_id,followup_version) WHERE followup_id IS NOT NULL;
