-- 诊断只由管理员读取；历史任务不回填推测值，发送正文与未发送草稿分开。
ALTER TABLE communication_takeover_jobs
    ADD COLUMN draft_answer TEXT,
    ADD COLUMN review_probability DOUBLE PRECISION CHECK(review_probability >= 0 AND review_probability <= 1),
    ADD COLUMN decision_threshold DOUBLE PRECISION CHECK(decision_threshold >= 0.5 AND decision_threshold <= 1);
