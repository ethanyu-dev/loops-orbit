import { useEffect, useRef, useState } from 'react';
import { api, ApiError } from '../../api';
import type { Detail } from '../../types';

// 活跃任务更快读取持久化片段，空闲时降低请求频率；同一轮询不重叠。
const ACTIVE_POLL_MS = 300;
const IDLE_POLL_MS = 1500;
const EMPTY_DETAIL: Detail = { messages: [], runs: [] };

/** 聊天数据生命周期及应用级回调。 */
interface ChatOptions {
  /** 当前会话，空值表示尚未创建。 */
  selected: string | null;
  /** 首条消息创建后同步导航。 */
  setSelected: (id: string) => void;
  /** 更新侧栏标题与排序。 */
  refreshList: () => Promise<void>;
  /** 统一展示故障并处理身份失效。 */
  report: (error: unknown) => void;
}

/** 未确认的提交保留原始幂等键，重试不重复接受同一条消息。 */
interface PendingSend {
  /** 提交所属会话。 */
  conversation: string | null;
  /** 用户发送时的原文快照。 */
  content: string;
  /** 一次发送动作的唯一身份。 */
  key: string;
}

/** 连续输入、取消与串行轮询；版本号阻止旧请求覆盖新消息或取消结果。 */
export function useChat({ selected, setSelected, refreshList, report }: ChatOptions) {
  const [detail, setDetail] = useState<Detail>(EMPTY_DETAIL);
  const [draft, setDraft] = useState('');
  const [sending, setSending] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [loading, setLoading] = useState(false);
  const [failedSends, setFailedSends] = useState<PendingSend[]>([]);
  const current = useRef(selected);
  const created = useRef<string | null>(null);
  const busy = useRef(false);
  const cancelling = useRef(false);
  const revision = useRef(0);
  current.current = selected;
  const pending = detail.runs.filter((r) => r.status === 'queued' || r.status === 'running');

  useEffect(() => {
    let active = true;
    let timer: ReturnType<typeof setTimeout>;
    const controller = new AbortController();
    revision.current++;
    setDetail(EMPTY_DETAIL);
    setLoading(!!selected);
    if (selected !== created.current) setDraft('');
    created.current = null;
    if (!selected) return;
    const load = async () => {
      let delay = IDLE_POLL_MS;
      try {
        if (!busy.current && !cancelling.current) {
          const version = ++revision.current;
          const result = await api<Detail>(`/conversations/${selected}`, {
            signal: controller.signal,
          });
          if (active && version === revision.current) {
            setDetail(result);
            setLoading(false);
          }
          if (result.runs.some((r) => r.status === 'queued' || r.status === 'running'))
            delay = ACTIVE_POLL_MS;
        } else {
          delay = ACTIVE_POLL_MS;
        }
      } catch (error) {
        if (!active) return;
        setLoading(false);
        report(error);
        if (error instanceof ApiError && [401, 403, 404].includes(error.status)) return;
      }
      if (active) timer = setTimeout(() => void load(), delay);
    };
    void load();
    return () => {
      active = false;
      revision.current++;
      controller.abort();
      clearTimeout(timer);
    };
  }, [selected, report]);

  /** 写入后立即读取一次；之前发出的轮询不能恢复旧的运行状态。 */
  async function reload(id: string) {
    const version = ++revision.current;
    const result = await api<Detail>(`/conversations/${id}`);
    if (current.current === id && version === revision.current) {
      setDetail(result);
      setLoading(false);
    }
  }

  /** 提交期间仍可编辑下一条草稿，失败的原消息单独重试，不覆盖新输入。 */
  async function send(retry?: PendingSend) {
    const content = retry?.content ?? draft.trim();
    if (!content || busy.current || cancelling.current) return;
    busy.current = true;
    revision.current++;
    setSending(true);
    if (!retry) setDraft('');
    let request: PendingSend = retry ?? {
      conversation: selected,
      content,
      key: crypto.randomUUID(),
    };
    const source = selected;
    try {
      let id = retry?.conversation ?? selected;
      if (!id) {
        const result = await api<{ id: string }>('/conversations', { method: 'POST' });
        id = result.id;
        if (current.current === source) {
          created.current = id;
          current.current = id;
          setSelected(id);
        }
      }
      request = { ...request, conversation: id };
      await api(`/conversations/${id}/messages`, {
        method: 'POST',
        body: JSON.stringify({ content, idempotency_key: request.key }),
      });
      setFailedSends((previous) => previous.filter((item) => item.key !== request.key));
    } catch (error) {
      setFailedSends((previous) => [
        ...previous.filter((item) => item.key !== request.key),
        request,
      ]);
      report(error);
      return;
    } finally {
      busy.current = false;
      setSending(false);
    }
    // 提交已确认后，详情或侧栏读取失败只报告故障，不能把它标成发送失败。
    try {
      if (request.conversation && current.current === request.conversation)
        await reload(request.conversation);
      await refreshList();
    } catch (error) {
      report(error);
    }
  }

  /** 停止请求只携带点击时的运行 ID，不会误停之后的新一轮。 */
  async function stop() {
    const id = selected;
    const run = pending[0];
    if (!id || !run || cancelling.current || busy.current) return;
    cancelling.current = true;
    revision.current++;
    setStopping(true);
    try {
      await api(`/conversations/${id}/runs/${run.id}/cancel`, { method: 'POST' });
      await reload(id);
    } catch (error) {
      report(error);
    } finally {
      cancelling.current = false;
      setStopping(false);
    }
  }
  const retries = failedSends.filter((item) => item.conversation === selected);
  return {
    detail,
    draft,
    setDraft,
    sending,
    stopping,
    loading,
    pending,
    send: () => send(),
    stop,
    failedSends: retries,
    retrySend: (request: PendingSend) => send(request),
  };
}
