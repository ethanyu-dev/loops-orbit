import { test } from 'node:test';
import assert from 'node:assert/strict';
import { fileStatus } from '../src/features/communications/library.ts';
import { summaryNotice, canRetrySummary } from '../src/features/communications/summaryStatus.ts';
import { processingError } from '../src/features/communications/processingError.ts';

// 验证部分结果不会展示为完整成功，并说明未核验范围；不覆盖真实 React 点击和后端文件读取。
test('部分摘要显示覆盖缺口并允许手动重排', () => {
  const detail = { document: { summary_status: 'partial', summary_hash: 'hash', summary_error: 'communication_summary_quote_mismatch' }, summary: { items: [{}], rejected_count: 2, failed_chunk_count: 1 } };
  assert.equal(fileStatus(detail.document).label, '部分整理完成');
  assert.match(summaryNotice(detail), /2 个候选未通过核验/);
  assert.match(summaryNotice(detail), /部分消息尚未完成/);
  assert.equal(canRetrySummary(detail), true);
  assert.equal(canRetrySummary({ ...detail, source_enabled: false }), false);
});

// 验证失败、重试和运行状态的解释及按钮门控；不模拟实际重试计时或模型调用。
test('终止与自动重试状态不混同刷新动作', () => {
  const document = { summary_status: 'failed', summary_error: 'communication_summary_unknown_message' };
  assert.match(summaryNotice({ document, summary: null }), /已停止自动重试/);
  assert.doesNotMatch(summaryNotice({ document, summary: null }), /未发现事项|稍后刷新/);
  assert.equal(canRetrySummary({ document }), true);
  for (const status of ['pending', 'running', 'retry_wait', 'ready']) {
    assert.equal(canRetrySummary({ document: { ...document, summary_status: status } }), false);
  }
  assert.equal(fileStatus({ ...document, summary_status: 'running' }).label, '整理中');
  assert.equal(fileStatus({ ...document, summary_status: 'retry_wait' }).label, '等待重试');
  assert.match(summaryNotice({ document: { ...document, summary_status: 'retry_wait', summary_attempts: 2 } }), /2\/3/);
});

// 验证旧服务错误不再误报通用等待提示；不推断历史失败的具体模型输出。
test('引用错误的兼容文案保留原因', () => {
  for (const code of ['communication_summary_invalid_quote', 'communication_summary_quote_mismatch', 'communication_summary_unknown_message']) {
    assert.doesNotMatch(processingError(code), /稍后刷新/);
  }
  assert.match(summaryNotice({ document: { summary_error: 'communication_summary_invalid_quote' }, summary: null }), /引用无法/);
});
