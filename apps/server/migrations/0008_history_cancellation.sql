-- 取消停止历史拉取和每日复查，保留已导入资料；版本阻止取消前的在途响应重新提交。
ALTER TABLE communication_history_jobs DROP CONSTRAINT communication_history_jobs_status_check;
ALTER TABLE communication_history_jobs ADD CONSTRAINT communication_history_jobs_status_check CHECK(status IN ('pending','running','complete','failed','cancelled'));
ALTER TABLE communication_history_jobs ADD COLUMN version BIGINT NOT NULL DEFAULT 1;
