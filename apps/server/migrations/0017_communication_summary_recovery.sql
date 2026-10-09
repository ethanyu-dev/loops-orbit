-- 摘要状态与文件指纹分离，部分结果不能被视为完整成功；旧失败资料获得一次有界重试机会。
ALTER TABLE communication_documents
    ADD COLUMN summary_status TEXT NOT NULL DEFAULT 'pending'
        CHECK(summary_status IN ('pending','running','retry_wait','ready','partial','failed')),
    ADD COLUMN summary_attempts INTEGER NOT NULL DEFAULT 0 CHECK(summary_attempts >= 0);
UPDATE communication_documents SET summary_status='ready' WHERE summary_hash IS NOT NULL;

-- 所有原文与图片版本更新共用失效边界，避免某个写入口遗忘重置已耗尽的重试预算。
CREATE FUNCTION reset_communication_summary_job() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.version IS DISTINCT FROM OLD.version
       OR NEW.raw_hash IS DISTINCT FROM OLD.raw_hash
       OR NEW.extraction_version IS DISTINCT FROM OLD.extraction_version THEN
        NEW.summary_status := 'pending';
        NEW.summary_attempts := 0;
        NEW.summary_hash := NULL;
        NEW.summary_error := NULL;
        NEW.next_summary := now();
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER communication_summary_version_reset BEFORE UPDATE ON communication_documents
    FOR EACH ROW EXECUTE FUNCTION reset_communication_summary_job();

ALTER TABLE communication_document_progress DROP CONSTRAINT communication_document_progress_status_check;
ALTER TABLE communication_document_progress ADD CONSTRAINT communication_document_progress_status_check
    CHECK(status IN ('ready','partial','summarizing','indexing','errors'));

-- 领取与过期回收只扫描活跃状态，避免停止重试的历史错误持续占用队列索引。
DROP INDEX communication_summary_due;
CREATE INDEX communication_summary_jobs_due ON communication_documents(next_summary)
    WHERE summary_status IN ('pending','retry_wait','running');
