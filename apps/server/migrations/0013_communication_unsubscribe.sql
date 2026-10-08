-- 移除订阅可保留已有文件；已移除来源不得继续采集或被普通启停接口恢复。
ALTER TABLE communication_sources ADD COLUMN subscribed BOOLEAN NOT NULL DEFAULT true;
ALTER TABLE communication_sources ADD CONSTRAINT communication_unsubscribed_disabled CHECK (subscribed OR NOT enabled);
