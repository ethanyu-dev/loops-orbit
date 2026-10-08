import { test } from 'node:test';
import assert from 'node:assert/strict';
import { groupMonths, groupFiles, fileStatus, readOffset } from '../src/features/communications/library.ts';

// 验证目录使用服务端自然日跨月分组，不受运行机器时区影响；不覆盖实际文件或飞书导入。
test('日期目录跨月分组并保留同日多群文件', () => {
  const days = [{ day: '2026-10-01', count: 2 }, { day: '2026-09-30', count: 1 }];
  assert.deepEqual(groupMonths(days).map(group => group.month), ['2026-10', '2026-09']);
  const files = [{ id: 'a', day: days[0].day }, { id: 'b', day: days[1].day }, { id: 'c', day: days[0].day }];
  assert.deepEqual(groupFiles(files)[0].files.map(file => file.id), ['a', 'c']);
  assert.deepEqual(groupFiles([]), []);
});

// 核对和失败状态不能被旧摘要掩盖；仅验证展示优先级，不验证摘要文件是否真实存在。
test('文件整理状态区分核对、失败、完成和等待', () => {
  assert.equal(fileStatus({ extraction_version: 0, summary_hash: 'old' }).label, '核对中');
  assert.equal(fileStatus({ extraction_version: 1, summary_error: 'failed', summary_hash: 'old' }).label, '整理失败');
  assert.equal(fileStatus({ extraction_version: 1, summary_hash: 'ready' }).label, '已整理');
  assert.equal(fileStatus({ extraction_version: 1, summary_hash: null }).label, '待整理');
});

// URL 分页参数应有界；这里只验证浏览器归一化，后端参数验证由数据库路由测试覆盖。
test('分页拒绝负数、小数、无穷和超范围偏移', () => {
  for (const value of [null, '-1', '1.5', 'Infinity', 'NaN', '4294967296']) assert.equal(readOffset(value), 0);
  assert.equal(readOffset('100'), 100);
});
