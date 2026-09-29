-- 用户凭证只存认证加密后的密文；OAuth 状态与发起会话和回调浏览器绑定。
CREATE TABLE communication_connections (
    owner TEXT PRIMARY KEY CHECK(owner='admin'),
    open_id TEXT NOT NULL,
    name TEXT NOT NULL,
    credentials BYTEA NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    refresh_expires_at TIMESTAMPTZ NOT NULL,
    status TEXT NOT NULL DEFAULT 'active' CHECK(status IN ('active','reauthorize')),
    version BIGINT NOT NULL DEFAULT 1
);
CREATE TABLE communication_oauth_states (
    state_hash TEXT PRIMARY KEY,
    browser_hash TEXT NOT NULL,
    session_hash TEXT NOT NULL REFERENCES sessions(token_hash) ON DELETE CASCADE,
    expires_at TIMESTAMPTZ NOT NULL
);
-- 固定窗口及分页游标共同提交，只有完整遍历后才推进时间水位。
CREATE TABLE communication_sources (
    id UUID PRIMARY KEY,
    owner TEXT NOT NULL REFERENCES communication_connections(owner) ON DELETE CASCADE,
    chat_id TEXT NOT NULL,
    label TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT true,
    version BIGINT NOT NULL DEFAULT 1,
    start_at BIGINT NOT NULL,
    watermark BIGINT NOT NULL,
    window_start BIGINT,
    window_end BIGINT,
    page_token TEXT NOT NULL DEFAULT '',
    audit_at TIMESTAMPTZ NOT NULL DEFAULT now()+interval '1 day',
    next_sync TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_synced_at TIMESTAMPTZ,
    error TEXT,
    UNIQUE(owner,chat_id)
);
-- 文件是正文，数据库保存当前文件指纹和可恢复的派生作业状态。
CREATE TABLE communication_documents (
    id UUID PRIMARY KEY,
    source_id UUID NOT NULL REFERENCES communication_sources(id) ON DELETE CASCADE,
    day TEXT NOT NULL,
    raw_hash TEXT NOT NULL,
    version BIGINT NOT NULL DEFAULT 1,
    summary_hash TEXT,
    summary_error TEXT,
    next_summary TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(source_id,day)
);
CREATE INDEX communication_sync_due ON communication_sources(next_sync) WHERE enabled;
CREATE INDEX communication_summary_due ON communication_documents(next_summary) WHERE summary_hash IS NULL;
