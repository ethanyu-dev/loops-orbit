import { test } from 'node:test';
import assert from 'node:assert/strict';
import { conversationPath, migrateLegacyLocation } from '../src/layout/navigation.ts';

const ID = '12345678-1234-1234-1234-123456789abc';

// 验证旧片段可迁移为会话路径；不代表会话存在或当前身份有权访问。
test('旧会话书签迁移到独立路径', () => {
  assert.equal(migrateLegacyLocation(new URL(`https://orbit.example/#chat=${ID}`)), `/chat/${ID}`);
  assert.equal(conversationPath('not-a-uuid'), '/chat');
});

// 验证 OAuth 结果和旧资料引用保留查询参数；不覆盖真实飞书授权。
test('旧资料与 OAuth 地址落到沟通页', () => {
  for (const query of [`communication=${ID}`, 'feishu=connected']) {
    assert.equal(migrateLegacyLocation(new URL(`https://orbit.example/?${query}`)), `/communications?${query}`);
  }
});

// 验证刷新入口和未知地址不被改写为聊天；浏览器返回行为由实际路由测试核对。
test('保留功能页和未知路径', () => {
  for (const path of ['/memory', '/followups', '/links', '/status', '/missing', `/chat/${ID}`]) {
    assert.equal(migrateLegacyLocation(new URL(`https://orbit.example${path}`)), path);
  }
});
