import { test } from 'node:test';
import assert from 'node:assert/strict';
import { selectRetained, toggleRetained, retainedVersions } from '../src/features/communications/retainedSelection.ts';

// 验证跨页全选捕获版本，后续轮询不静默更新确认范围；不模拟真实浏览器或服务端并发。
test('历史选择跨页并固定版本，提交只包含身份与版本', () => {
  const rows = Array.from({ length: 60 }, (_, i) => ({ id: `${i}`, version: 2, label: `会话${i}`, subscribed: false }));
  const selected = selectRetained(rows);
  rows[0].version = 3;
  rows.push({ id: 'new', version: 1, label: '新会话', subscribed: false });
  assert.equal(selected.length, 60);
  assert.deepEqual(retainedVersions(selected)[0], { id: '0', version: 2 });
  assert.equal(toggleRetained(selected, rows[0]).length, 59);
});

// 验证筛选后的显式范围、已订阅排除与批量上限；不验证后端认证和数据库版本核对。
test('全选只包含历史来源且有数量上限', () => {
  const rows = Array.from({ length: 1005 }, (_, i) => ({ id: `${i}`, version: 1, label: '历史', subscribed: false }));
  assert.equal(selectRetained(rows).length, 1000);
  const active = { id: 'active', version: 1, label: '已订阅', subscribed: true };
  assert.deepEqual(selectRetained([active]), []);
  assert.deepEqual(toggleRetained([], active), []);
  // 后台删除已经受理的来源不能再被全选或单独勾选。
  const pending = { ...rows[0], removal_pending: true };
  assert.deepEqual(selectRetained([pending]), []);
  assert.deepEqual(toggleRetained([], pending), []);
  const selected = selectRetained(rows);
  assert.equal(toggleRetained(selected, rows[1001]).length, 1000);
  assert.deepEqual(retainedVersions(selectRetained(rows.filter((row) => row.id === '42'))), [{ id: '42', version: 1 }]);
});
