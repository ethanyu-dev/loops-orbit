-- 处理进度是可重建的后台快照；管理页不能为显示连接状态遍历所有原文文件。
CREATE TABLE communication_document_progress (
    document_id UUID PRIMARY KEY REFERENCES communication_documents(id) ON DELETE CASCADE,
    version BIGINT NOT NULL,
    summary_hash TEXT,
    summary_error TEXT,
    embedding_version TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('ready','summarizing','indexing','errors')),
    images BIGINT NOT NULL,
    images_ready BIGINT NOT NULL,
    images_failed BIGINT NOT NULL,
    checked_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX communication_progress_checked ON communication_document_progress(checked_at);
