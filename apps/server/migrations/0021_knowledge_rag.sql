-- 原始快照独立留存，无沟通文件外键；删除源文件不会级联删除已审核知识的依据。
CREATE TABLE knowledge_snapshots (
    id UUID PRIMARY KEY,
    origin_id UUID NOT NULL,
    origin_version BIGINT NOT NULL,
    raw_hash TEXT NOT NULL,
    source_label TEXT NOT NULL,
    source_day TEXT NOT NULL,
    messages JSONB NOT NULL,
    captured_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(origin_id,origin_version,raw_hash)
);
ALTER TABLE knowledge_entries ADD COLUMN tags TEXT[] NOT NULL DEFAULT '{}';

-- 同一 PG 保存正文与可重建的检索派生物；部署镜像必须提供 pgvector。
CREATE EXTENSION IF NOT EXISTS vector WITH SCHEMA public;
CREATE TABLE rag_chunks (
    id UUID PRIMARY KEY,
    scope TEXT NOT NULL CHECK(scope IN ('published','private')),
    knowledge_id UUID REFERENCES knowledge_entries(id) ON DELETE CASCADE,
    document_id UUID REFERENCES communication_documents(id) ON DELETE CASCADE,
    revision BIGINT NOT NULL,
    source_hash TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    payload JSONB NOT NULL DEFAULT '{}',
    terms TEXT NOT NULL,
    lexemes TSVECTOR GENERATED ALWAYS AS (to_tsvector('simple'::regconfig,terms)) STORED,
    embedding public.vector,
    embedding_version TEXT,
    embedding_retry_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    embedding_failures INTEGER NOT NULL DEFAULT 0,
    CHECK((scope='published' AND knowledge_id IS NOT NULL AND document_id IS NULL)
       OR (scope='private' AND document_id IS NOT NULL AND knowledge_id IS NULL)),
    UNIQUE(knowledge_id,ordinal),
    UNIQUE(document_id,ordinal)
);
CREATE INDEX rag_lexical ON rag_chunks USING GIN(lexemes);
CREATE INDEX rag_embedding_queue ON rag_chunks(embedding_retry_at);
-- 空文件也记录索引边界，避免后台反复读取。源文件删除只清理私有派生索引。
CREATE TABLE rag_documents (
    document_id UUID PRIMARY KEY REFERENCES communication_documents(id) ON DELETE CASCADE,
    revision BIGINT NOT NULL,
    source_hash TEXT NOT NULL
);

-- 单份坏文件独立退避，不能阻止其他文件建立索引。
CREATE TABLE rag_index_retry (
    document_id UUID PRIMARY KEY REFERENCES communication_documents(id) ON DELETE CASCADE,
    retry_at TIMESTAMPTZ NOT NULL
);

-- 权限和版本在召回前过滤；暂停、撤回、编辑均立即使旧索引不可见。
CREATE VIEW rag_eligible AS
SELECT r.* FROM rag_chunks r
LEFT JOIN knowledge_entries k ON r.knowledge_id=k.id
LEFT JOIN communication_documents d ON r.document_id=d.id
LEFT JOIN communication_sources s ON d.source_id=s.id
WHERE (r.scope='published' AND k.status='published' AND k.version=r.revision)
   OR (r.scope='private' AND d.version=r.revision AND d.raw_hash||':'||COALESCE(d.summary_hash,'')=r.source_hash
       AND d.extraction_version=1 AND s.enabled AND s.owner='admin' AND NOT s.removal_pending);
