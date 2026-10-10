-- 将已提交的待办操作关联到真实运行；旧操作保留空值，不猜测历史来源。
ALTER TABLE todo_operations ADD COLUMN run_id UUID REFERENCES runs(id) ON DELETE SET NULL;
CREATE INDEX todo_operations_run ON todo_operations(run_id) WHERE run_id IS NOT NULL;
