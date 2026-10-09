-- 自聊测试默认关闭；会话只能由当前用户令牌向本人发送提示后绑定。
ALTER TABLE communication_takeover_settings ADD COLUMN self_test_chat_id TEXT;
