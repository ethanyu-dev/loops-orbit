-- 文件为原文；这里只记录遗忘边界、抽取队列和索引元数据。
CREATE TABLE memory_owners (
    owner TEXT PRIMARY KEY,
    forgotten_through BIGINT NOT NULL DEFAULT 0
);
CREATE TABLE memory_jobs (
    run_id UUID PRIMARY KEY REFERENCES runs(id),
    owner TEXT NOT NULL,
    source_seq BIGINT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    available_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    status TEXT NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','completed','failed'))
);
CREATE INDEX memory_jobs_pending ON memory_jobs(available_at) WHERE status='queued';
-- 老会话不自动回填，避免升级时把过去所有私人内容未经筛选重新提取。
