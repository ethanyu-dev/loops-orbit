-- 授权只保存摘要，链接撤销和过期在每次会话认证时生效。
CREATE TABLE grants (
    id UUID PRIMARY KEY,
    token_hash TEXT NOT NULL UNIQUE,
    label TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE sessions (
    token_hash TEXT PRIMARY KEY,
    grant_id UUID REFERENCES grants(id),
    admin_fingerprint TEXT,
    expires_at TIMESTAMPTZ NOT NULL,
    CHECK ((grant_id IS NULL) <> (admin_fingerprint IS NULL))
);
CREATE TABLE conversations (
    id UUID PRIMARY KEY,
    owner TEXT NOT NULL,
    channel TEXT NOT NULL CHECK (channel IN ('web', 'feishu')),
    external_key TEXT UNIQUE,
    title TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX conversations_owner ON conversations(owner, updated_at DESC);
-- 单调序号决定同一会话的排队顺序；租约令牌防止旧执行器覆盖新结果。
CREATE TABLE runs (
    seq BIGSERIAL UNIQUE NOT NULL,
    id UUID PRIMARY KEY,
    conversation_id UUID NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    idempotency_key TEXT NOT NULL,
    input TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'queued' CHECK (status IN ('queued', 'running', 'completed', 'failed')),
    attempts INT NOT NULL DEFAULT 0,
    lease_token UUID,
    lease_until TIMESTAMPTZ,
    available_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    error TEXT,
    reply_to TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ,
    UNIQUE(conversation_id, idempotency_key)
);
CREATE INDEX runs_queue ON runs(status, available_at, seq);
CREATE INDEX runs_conversation ON runs(conversation_id, seq);
CREATE TABLE messages (
    seq BIGSERIAL PRIMARY KEY,
    conversation_id UUID NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    run_id UUID NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('user', 'assistant')),
    content TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(run_id, role)
);
CREATE INDEX messages_conversation ON messages(conversation_id, seq);
-- 飞书事件去重与回复出站队列分别持久化，模型完成后不因发送失败而重复生成。
CREATE TABLE feishu_events (id TEXT PRIMARY KEY, created_at TIMESTAMPTZ NOT NULL DEFAULT now());
CREATE TABLE outbox (
    id UUID PRIMARY KEY,
    reply_to TEXT NOT NULL,
    content TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'queued',
    attempts INT NOT NULL DEFAULT 0,
    available_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    lease_token UUID,
    lease_until TIMESTAMPTZ,
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX outbox_queue ON outbox(status, available_at);
-- 登录限流使用共享计数，跨实例有效；不信任客户端声明的源 IP。
CREATE TABLE rate_limits (bucket TEXT PRIMARY KEY, hits INT NOT NULL, expires_at TIMESTAMPTZ NOT NULL);
