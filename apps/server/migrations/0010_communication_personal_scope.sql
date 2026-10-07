-- 新连接默认手动订阅；停止旧连接的自动发现，不擅自移除已有来源。
ALTER TABLE communication_connections ALTER COLUMN auto_subscribe SET DEFAULT false;
UPDATE communication_connections SET auto_subscribe=false,version=version+1 WHERE auto_subscribe;
-- 会话类型来自飞书，不接受浏览器声明的单聊身份。
ALTER TABLE communication_sources ADD COLUMN chat_mode TEXT;
-- 旧资料在个人关联范围重新核对完成前，不参与摘要、图片解读和检索。
ALTER TABLE communication_documents ADD COLUMN extraction_version BIGINT NOT NULL DEFAULT 0;
ALTER TABLE communication_documents ALTER COLUMN extraction_version SET DEFAULT 1;
ALTER TABLE communication_documents ADD COLUMN next_extraction TIMESTAMPTZ NOT NULL DEFAULT now();
-- 启动时先清理旧推理上下文，再接受请求；完成标记避免每次启动重复清理。
ALTER TABLE communication_connections ADD COLUMN scope_context_pending BOOLEAN NOT NULL DEFAULT false;
UPDATE communication_connections SET scope_context_pending=true WHERE EXISTS(SELECT 1 FROM communication_documents WHERE extraction_version=0);
