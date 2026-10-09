import { useEffect, useState } from 'react';
import { Link } from 'react-router-dom';
import { BookOpen } from 'lucide-react';
import { api, errorText } from '../../api';
import { Spinner } from '../../components/Feedback';
import type { ExtractionTask } from './types';

// 只轮询已受理的任务，不因浏览文件自动触发提取。
const POLL_MS = 2000;

/** 单个日文件的显式提取入口；与摘要重试分开，所有产出仍需本人发布。 */
export function KnowledgeExtraction({
  documentId,
  version,
  day,
}: {
  /** 只在受理时定位当前选择，不作为知识的存储依赖。 */
  documentId: string;
  /** 提交浏览时的版本，后台变化时要求刷新后重新选择。 */
  version: number;
  /** 明确此次提取覆盖的北京时间自然日。 */
  day: string;
}) {
  const [task, setTask] = useState<ExtractionTask | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [refresh, setRefresh] = useState(0);
  const pending = task?.status === 'queued' || task?.status === 'running';
  const taskId = task?.id;
  useEffect(() => {
    if (!pending || !taskId) return;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    /** 下一次读取等前一次结束再安排，离开页面停止轮询。 */
    async function poll() {
      try {
        const next = await api<ExtractionTask>(`/knowledge/extractions/${taskId}`, {
          signal: controller.signal,
        });
        if (controller.signal.aborted) return;
        setTask(next);
        setError('');
        if (next.status === 'queued' || next.status === 'running')
          timer = setTimeout(() => void poll(), POLL_MS);
      } catch (cause) {
        if (!controller.signal.aborted) setError(errorText(cause));
      }
    }
    timer = setTimeout(() => void poll(), POLL_MS);
    return () => {
      controller.abort();
      clearTimeout(timer);
    };
  }, [pending, taskId, refresh]);

  /** 重复点击由服务端按文件版本合并，失败时只恢复未完成分块。 */
  async function start() {
    setBusy(true);
    setError('');
    try {
      setTask(
        await api<ExtractionTask>('/knowledge/extractions', {
          method: 'POST',
          body: JSON.stringify({ document_id: documentId, version }),
        }),
      );
    } catch (cause) {
      setError(errorText(cause));
    } finally {
      setBusy(false);
    }
  }
  return (
    <section className="document-section" aria-labelledby="document-knowledge-title">
      <div className="document-section-heading">
        <h2 id="document-knowledge-title">沉淀为通用知识</h2>
        <span>仅保存候选 · 发布需你确认</span>
      </div>
      <p className="document-note">
        由 agent 从此会话 {day} 的完整原文中筛选可复用的事实、流程和经验。保存后的知识独立于原文件。
      </p>
      <div className="document-empty memory-actions">
        <button
          disabled={busy || pending || task?.status === 'completed'}
          onClick={() => void start()}
        >
          {busy || pending ? <Spinner /> : <BookOpen size={15} />}
          {busy
            ? '正在提交…'
            : pending
              ? '正在提取…'
              : task?.status === 'completed'
                ? '此版本已提取'
                : task?.status === 'failed'
                  ? '重试知识提取'
                  : '提取候选知识'}
        </button>
        <Link to="/knowledge">查看通用知识库</Link>
      </div>
      {task && (
        <p role="status" className="document-note">
          {task.status === 'completed'
            ? task.created_count > 0
              ? `提取完成，新增 ${task.created_count} 条待确认知识。`
              : '提取完成，未新增候选：没有发现适合沉淀的内容，或候选已存在。'
            : task.status === 'failed'
              ? '提取失败，可重试继续处理。'
              : task.status === 'queued'
                ? '已排队，无需等待每日定时扫描。'
                : `正在处理，已读取 ${task.next_offset} 条消息。`}
          {task.skipped_count > 0 && ` ${task.skipped_count} 条超长消息未参与提取。`}
        </p>
      )}
      {error && (
        <p role="alert" className="document-note">
          {error}
          {pending && (
            <button onClick={() => setRefresh((value) => value + 1)}>重试读取状态</button>
          )}
        </p>
      )}
    </section>
  );
}
