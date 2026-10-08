-- 私聊自动订阅使用独立开关，旧版全量自动订阅继续关闭，避免扩大到群聊。
ALTER TABLE communication_connections ADD COLUMN auto_subscribe_private BOOLEAN NOT NULL DEFAULT true;
-- 迁移时以当前时间为历史边界，新授权连接以创建时间为边界，不自动回溯旧聊天。
ALTER TABLE communication_connections ADD COLUMN private_subscription_since BIGINT NOT NULL DEFAULT extract(epoch FROM now())::bigint;
-- 断开后重新授权可能重置版本号，独立代际避免旧发现结果写入新连接。
ALTER TABLE communication_connections ADD COLUMN private_discovery_generation UUID NOT NULL DEFAULT gen_random_uuid();
ALTER TABLE communication_connections ADD COLUMN discovery_pages INTEGER NOT NULL DEFAULT 0;
UPDATE communication_connections SET discovery_cursor='',discovery_pages=0,next_discovery=now(),discovery_error=NULL;
