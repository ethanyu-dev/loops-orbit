-- Linear 首版只绑定网页管理员，访客及飞书身份不会继承管理员授权。
CREATE TABLE linear_connections (
    owner TEXT PRIMARY KEY CHECK(owner='admin'),
    generation UUID NOT NULL,
    user_id TEXT NOT NULL,
    user_name TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    workspace_name TEXT NOT NULL,
    workspace_slug TEXT NOT NULL,
    credentials BYTEA NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    scopes TEXT[] NOT NULL,
    status TEXT NOT NULL DEFAULT 'active' CHECK(status IN ('active','reauthorize'))
);
CREATE TABLE linear_guard (id BOOLEAN PRIMARY KEY CHECK(id), version BIGINT NOT NULL);
INSERT INTO linear_guard VALUES(true,1);
CREATE TABLE linear_oauth_states (
    state_hash TEXT PRIMARY KEY,
    browser_hash TEXT NOT NULL,
    session_hash TEXT NOT NULL REFERENCES sessions(token_hash) ON DELETE CASCADE,
    verifier TEXT NOT NULL,
    version BIGINT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL
);
-- 外部写操作在请求发出之前落盘；未知结果不可因 worker 重试而再次发送。
CREATE TABLE linear_operations (
    operation_key TEXT PRIMARY KEY,
    run_id UUID NOT NULL REFERENCES runs(id),
    generation UUID NOT NULL,
    issue_id TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('dispatching','confirmed','unknown')),
    result JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
