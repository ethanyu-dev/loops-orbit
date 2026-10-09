-- 保持历史迁移和已有 OAuth 行兼容；只有匹配当前密钥摘要的连接才会启用。
-- 个人密钥始终留在环境配置中，不写入数据库，也不依赖旧 OAuth 加密密钥。
ALTER TABLE linear_connections ADD COLUMN key_fingerprint TEXT;
ALTER TABLE linear_connections ALTER COLUMN credentials DROP NOT NULL;
ALTER TABLE linear_connections ALTER COLUMN expires_at DROP NOT NULL;

-- 明确的认证、权限或限流拒绝与无法确定结果的网络故障分别记录。
ALTER TABLE linear_operations DROP CONSTRAINT linear_operations_status_check;
ALTER TABLE linear_operations ADD CHECK(status IN ('dispatching','confirmed','unknown','rejected'));
