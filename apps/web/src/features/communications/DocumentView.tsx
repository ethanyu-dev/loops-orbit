import { useEffect, useState, type FormEvent } from 'react';
import { ArrowLeft, Bell, RefreshCw } from 'lucide-react';
import { API_ORIGIN } from '../../config';
import { api, errorText } from '../../api';
import { useSearchParams } from 'react-router-dom';
import { processingError } from './processingError';
import { canRetrySummary, summaryNotice } from './summaryStatus';
import type { Detail, Item } from './types';

// 分类展示不暗示机器摘要已被用户确认。
const LABELS: Record<string, string> = {
  decision: '决定',
  my_commitment: '我的承诺',
  their_commitment: '对方的承诺',
  open_question: '待确认',
  fact_candidate: '待确认的长期信息',
};
/** 原文和摘要并列核对，只有显式提交表单才创建提醒。 */
export function DocumentView({
  id,
  report,
  onClose,
}: {
  id: string;
  report: (e: unknown) => void;
  onClose: () => void;
}) {
  const [params, setParams] = useSearchParams();
  const imageErrors = params.get('image_errors') === 'true';
  const [loadError, setLoadError] = useState('');
  const [revision, setRevision] = useState(0);
  const [detail, setDetail] = useState<Detail | null>(null);
  const [offset, setOffset] = useState(0);
  const [item, setItem] = useState<number | null>(null);
  const [topic, setTopic] = useState('');
  const [due, setDue] = useState('');
  const [kind, setKind] = useState('reminder');
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState('');
  const [key, setKey] = useState(() => crypto.randomUUID());
  // 切页时取消旧请求，错误留在详情页内供重试。
  useEffect(() => {
    const controller = new AbortController();
    setDetail(null);
    setLoadError('');
    setItem(null);
    void api<Detail>(
      `/communications/documents/${id}?offset=${offset}&image_errors=${imageErrors}`,
      {
        signal: controller.signal,
      },
    )
      .then((value) => {
        if (!controller.signal.aborted) setDetail(value);
      })
      .catch((error) => {
        if (!controller.signal.aborted) setLoadError(errorText(error));
      });
    return () => controller.abort();
  }, [id, offset, revision, imageErrors]);
  /** 重新整理是显式写操作；刷新只更新详情读取版本。 */
  async function retrySummary() {
    if (!detail) return;
    setBusy(true);
    try {
      await api(`/communications/documents/${id}/summary/retry`, {
        method: 'POST',
        body: JSON.stringify({ version: detail.document.version }),
      });
      setNotice('已排队重新整理。');
      setRevision((value) => value + 1);
    } catch (error) {
      report(error);
      setRevision((value) => value + 1);
    } finally {
      setBusy(false);
    }
  }
  /** 归纳文本可修正；选择另一个条目后更换幂等键。 */
  function select(value: Item, index: number) {
    setItem(index);
    setTopic(value.text);
    setNotice('');
    setKey(crypto.randomUUID());
  }
  /** 时间转换为带偏移的绝对时刻，页面明确使用浏览器本地时区。 */
  async function create(event: FormEvent) {
    event.preventDefault();
    if (item === null || !detail) return;
    setBusy(true);
    try {
      await api('/followups', {
        method: 'POST',
        body: JSON.stringify({
          idempotency_key: key,
          kind,
          topic,
          due_at: new Date(due).toISOString(),
          communication: { document_id: id, version: detail.document.version, item },
        }),
      });
      setNotice('已加入提醒与跟进。来源被修正、暂停或遗忘时，此事项会停止。');
      setItem(null);
    } catch (e) {
      report(e);
    } finally {
      setBusy(false);
    }
  }
  return (
    <div className="settings-page communication-detail-page">
      <section className="communication-card">
        <button className="detail-back" onClick={onClose}>
          <ArrowLeft size={16} />
          沟通记录
        </button>
        <header className="document-heading">
          <div>
            <h1>{detail?.source_label || '沟通资料'}</h1>
            <p className="document-meta">
              {detail?.document.day || '正在读取'}
              {detail && !detail.processing
                ? ` · ${detail.total} 条${imageErrors ? '含失败图片的' : '相关'}消息`
                : ''}
            </p>
          </div>
          <button className="document-refresh" onClick={() => setRevision((value) => value + 1)}>
            <RefreshCw size={15} />
            刷新
          </button>
        </header>
        {notice && <p role="status">{notice}</p>}
        {loadError ? (
          <div role="alert">
            <p>{loadError}</p>
            <button onClick={() => setRevision((value) => value + 1)}>重试读取资料</button>
          </div>
        ) : !detail ? (
          <p role="status">正在读取整理与原文…</p>
        ) : detail.processing ? (
          <p role="status">
            正在按“与你相关”的规则重新核对旧资料，完成后可查看。
            <button onClick={() => setRevision((value) => value + 1)}>刷新状态</button>
          </p>
        ) : (
          <>
            {!imageErrors && (
              <section className="document-section" aria-labelledby="document-summary-title">
                <div className="document-section-heading">
                  <h2 id="document-summary-title">整理结果</h2>
                  <span>AI 归纳 · 请核对原话</span>
                </div>
                {summaryNotice(detail) && (
                  <p className="document-empty" role="status">
                    {summaryNotice(detail)}
                  </p>
                )}
                {canRetrySummary(detail) && (
                  <div className="document-empty">
                    <button disabled={busy} onClick={() => void retrySummary()}>
                      {busy ? '正在提交…' : '重新整理'}
                    </button>
                    {detail.document.summary_status === 'partial' && (
                      <p>重新整理会替换当前结果，并停止依赖当前结果的提醒与跟进。</p>
                    )}
                  </div>
                )}
                {detail.summary?.items.length === 0 && !detail.document.summary_error && (
                  <p className="document-empty">暂未发现明确的决定、承诺或待确认事项。</p>
                )}
                {detail.summary?.items.map((value, index) => (
                  <article className="summary-item" key={`${value.message_id}-${index}`}>
                    <span className="summary-kind">{LABELS[value.kind] || value.kind}</span>
                    <p className="summary-text">{value.text}</p>
                    <details className="summary-evidence">
                      <summary>查看引用原话</summary>
                      <blockquote>{value.quote}</blockquote>
                    </details>
                    <div className="summary-footer">
                      <span>
                        {value.sender_name || (value.is_me ? '我' : '成员（姓名未获取）')} ·{' '}
                        {new Date(value.create_time).toLocaleTimeString('zh-CN', {
                          timeZone: 'Asia/Shanghai',
                          hour: '2-digit',
                          minute: '2-digit',
                          hour12: false,
                        })}
                      </span>
                      <button onClick={() => select(value, index)}>
                        <Bell size={14} />
                        设为提醒
                      </button>
                    </div>
                  </article>
                ))}
              </section>
            )}
            {item !== null && (
              <form className="document-reminder-form" onSubmit={(event) => void create(event)}>
                <h3>确认跟进事项</h3>
                <label>
                  要提醒我的事
                  <input
                    value={topic}
                    maxLength={500}
                    onChange={(e) => {
                      setTopic(e.target.value);
                      setKey(crypto.randomUUID());
                    }}
                    required
                  />
                </label>
                <label>
                  方式
                  <select
                    value={kind}
                    onChange={(e) => {
                      setKind(e.target.value);
                      setKey(crypto.randomUUID());
                    }}
                  >
                    <option value="reminder">到时提醒</option>
                    <option value="checkin">轻量回访（需已开启主动回访）</option>
                  </select>
                </label>
                <label>
                  时间（{Intl.DateTimeFormat().resolvedOptions().timeZone}）
                  <input
                    type="datetime-local"
                    value={due}
                    required
                    onChange={(e) => {
                      setDue(e.target.value);
                      setKey(crypto.randomUUID());
                    }}
                  />
                </label>
                <button className="primary" disabled={busy}>
                  确认安排
                </button>
                <button type="button" onClick={() => setItem(null)}>
                  取消
                </button>
              </form>
            )}
            <section className="document-section" aria-labelledby="document-messages-title">
              <div className="document-section-heading">
                <h2 id="document-messages-title">
                  {imageErrors ? '图片解读失败的消息' : '原始消息'}
                </h2>
                <span>{detail.total} 条 · 北京时间</span>
              </div>
              <label className="document-error-toggle">
                <input
                  type="checkbox"
                  checked={imageErrors}
                  onChange={(event) => {
                    const next = new URLSearchParams(params);
                    if (event.target.checked) next.set('image_errors', 'true');
                    else next.delete('image_errors');
                    setOffset(0);
                    setParams(next, { replace: true });
                  }}
                />
                只看图片解读失败的消息
              </label>
              {detail.summary && detail.summary.unsupported_count > 0 && (
                <p className="document-note">
                  {detail.summary.unsupported_count} 条消息未参与文字摘要，图片解读单独展示。
                </p>
              )}
              {detail.total === 0 && (
                <p>
                  {imageErrors
                    ? '当前没有解读失败的图片，可能已自动重试成功。'
                    : '这一天没有符合个人关联规则的消息。'}
                </p>
              )}
              {detail.messages.map((message) => (
                <article key={message.message_id} className="document-message">
                  <div className="document-message-meta">
                    <strong>
                      {message.sender_name || (message.is_me ? '我' : '成员（姓名未获取）')}
                    </strong>
                    {message.relation && (
                      <span className="message-relation">{message.relation}</span>
                    )}
                    <time>
                      {new Date(message.create_time).toLocaleTimeString('zh-CN', {
                        timeZone: 'Asia/Shanghai',
                        hour: '2-digit',
                        minute: '2-digit',
                        hour12: false,
                      })}
                    </time>
                  </div>
                  <p className="document-message-text">
                    {message.deleted
                      ? '消息已撤回'
                      : message.text ||
                        (message.images?.length
                          ? '［图片消息］'
                          : message.message_type === 'interactive'
                            ? '［交互卡片：未包含可提取的文字］'
                            : `［${message.message_type}：暂不支持内容解析］`)}
                  </p>
                  {message.images?.map((image, index) => (
                    <div className="communication-image" key={image.url}>
                      <a href={`${API_ORIGIN}${image.url}`} target="_blank" rel="noreferrer">
                        查看原图 {message.images.length > 1 ? index + 1 : ''}
                      </a>
                      <p>
                        {image.description
                          ? `图片机器解读：${image.description}`
                          : image.reference_only
                            ? '仅保存原图入口；单条消息自动解读前 20 张图片。'
                            : image.error
                              ? `图片解读失败：${processingError(image.error)}（${image.error}）。可查看原图核对。`
                              : '图片待解读，原图入口已保存。'}
                      </p>
                    </div>
                  ))}
                </article>
              ))}
              {detail.total > 50 && (
                <div className="communication-row document-pagination">
                  <button
                    disabled={offset === 0}
                    onClick={() => setOffset(Math.max(0, offset - 50))}
                  >
                    上一页
                  </button>
                  <span>
                    {detail.total ? offset + 1 : 0}–{Math.min(offset + 50, detail.total)} /{' '}
                    {detail.total}
                  </span>
                  <button
                    disabled={offset + 50 >= detail.total}
                    onClick={() => setOffset(offset + 50)}
                  >
                    下一页
                  </button>
                </div>
              )}
            </section>
            <p className="document-footnote">
              仅整理与你相关的消息；机器归纳不会自动写入个人记忆或创建提醒。
            </p>
          </>
        )}
      </section>
    </div>
  );
}
