-- 自动发现只从启用时刻采集新增消息，历史补录使用独立队列。
ALTER TABLE communication_connections ADD COLUMN auto_subscribe BOOLEAN NOT NULL DEFAULT true;
ALTER TABLE communication_connections ADD COLUMN subscription_since BIGINT NOT NULL DEFAULT extract(epoch FROM now())::bigint;
ALTER TABLE communication_connections ADD COLUMN discovery_cursor TEXT NOT NULL DEFAULT '';
ALTER TABLE communication_connections ADD COLUMN next_discovery TIMESTAMPTZ NOT NULL DEFAULT now();
ALTER TABLE communication_connections ADD COLUMN discovery_error TEXT;
-- 已有文件先保持 UTC 标记，后台原子重分组后再切换北京时间。
ALTER TABLE communication_sources ADD COLUMN day_timezone TEXT NOT NULL DEFAULT 'UTC';
CREATE TABLE communication_exclusions (
    owner TEXT NOT NULL REFERENCES communication_connections(owner) ON DELETE CASCADE,
    chat_id TEXT NOT NULL,
    PRIMARY KEY(owner,chat_id)
);
CREATE TABLE communication_history_jobs (
    id UUID PRIMARY KEY,
    source_id UUID NOT NULL REFERENCES communication_sources(id) ON DELETE CASCADE,
    start_at BIGINT NOT NULL,
    end_at BIGINT NOT NULL,
    snapshot_end BIGINT NOT NULL DEFAULT extract(epoch FROM now())::bigint,
    page_token TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','running','complete','failed')),
    next_attempt TIMESTAMPTZ NOT NULL DEFAULT now(),
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(source_id,start_at,end_at),
    CHECK(start_at<end_at)
);
-- 图片解读是派生资料，删除来源时级联清理；指纹防止编辑后使用旧解读。
CREATE TABLE communication_images (
    source_id UUID NOT NULL REFERENCES communication_sources(id) ON DELETE CASCADE,
    message_id TEXT NOT NULL,
    image_key TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    description TEXT,
    error TEXT,
    next_attempt TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY(source_id,message_id,image_key)
);
