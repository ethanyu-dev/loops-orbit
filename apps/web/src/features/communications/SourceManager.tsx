import { useRef, useState } from 'react';
import {
  ChevronDown,
  ChevronRight,
  Pause,
  Play,
  Plus,
  RefreshCw,
  Search,
  Trash2,
} from 'lucide-react';
import { api, errorText } from '../../api';
import { SourcePicker } from './SourcePicker';
import { RemoveSubscriptionDialog, type RemovalTarget } from './RemoveSubscriptionDialog';
import type { Snapshot, Source } from './types';
import './subscriptions.css';

// 紧凑列表按页渲染；全部移除以完整订阅快照为准，与本页和搜索条件无关。
const SOURCE_PAGE_SIZE = 25;

/** 订阅概况保持单行，失败原因和派生进度在展开区查看。 */
function status(source: Source) {
  if (source.subscribed === false) return '已移除';
  if (!source.enabled) return '已暂停';
  if (source.error) return '同步失败';
  if (source.window_end) return '同步中';
  return source.last_synced_at ? '已同步' : '等待同步';
}

/** 订阅管理集中处理单项与批量操作，候选选择按需展开，保留资料有独立入口。 */
export function SourceManager({
  data,
  reload,
  report,
}: {
  /** 轮询快照，包括保留资料的已移除来源。 */
  data: Snapshot;
  /** 变更后读取实际服务端状态。 */
  reload: () => Promise<void>;
  /** 普通操作的全局错误反馈。 */
  report: (error: unknown) => void;
}) {
  const [query, setQuery] = useState('');
  const [page, setPage] = useState(0);
  const [archived, setArchived] = useState(false);
  const [adding, setAdding] = useState(false);
  const [expanded, setExpanded] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const lock = useRef(false);
  const [target, setTarget] = useState<RemovalTarget | null>(null);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const subscriptions = data.sources.filter((source) => source.subscribed !== false);
  const retained = data.sources.filter((source) => source.subscribed === false);
  const progress = new Map(data.progress.map((item) => [item.source_id, item]));
  const filtered = (archived ? retained : subscriptions)
    .filter((source) => source.label.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase()))
    .sort((a, b) => a.label.localeCompare(b.label, 'zh-CN'));
  const currentPage = Math.min(
    page,
    Math.max(0, Math.ceil(filtered.length / SOURCE_PAGE_SIZE) - 1),
  );
  const visible = filtered.slice(
    currentPage * SOURCE_PAGE_SIZE,
    (currentPage + 1) * SOURCE_PAGE_SIZE,
  );

  /** 启停、重试和重新订阅不会隐式删除任何本地文件。 */
  async function change(source: Source, action: 'toggle' | 'sync' | 'restore') {
    if (lock.current) return;
    lock.current = true;
    setBusy(true);
    setNotice('');
    try {
      await api(
        `/communications/sources${action === 'restore' ? '' : `/${source.id}${action === 'sync' ? '/sync' : ''}`}`,
        {
          method: action === 'toggle' ? 'PUT' : 'POST',
          body:
            action === 'sync'
              ? undefined
              : JSON.stringify(
                  action === 'restore'
                    ? { chat_id: source.chat_id, label: source.label }
                    : { version: source.version, enabled: !source.enabled },
                ),
        },
      );
      await reload();
      setNotice(
        action === 'restore'
          ? '已重新订阅，保留的资料仍可查看。'
          : action === 'sync'
            ? '已安排同步最新消息。'
            : '已更新同步状态。',
      );
    } catch (error) {
      report(error);
    } finally {
      lock.current = false;
      setBusy(false);
    }
  }
  /** 弹窗显式提交；失败刷新快照但不静默扩大原先确认的范围。 */
  async function remove(deleteDocuments: boolean) {
    if (!target || lock.current) return;
    lock.current = true;
    setBusy(true);
    setError('');
    setNotice('');
    try {
      const result = await api<{ removed: number; failed_ids: string[] }>(
        '/communications/sources/remove',
        {
          method: 'POST',
          body: JSON.stringify({ selection: target.selection, delete_documents: deleteDocuments }),
        },
      );
      setNotice(
        result.failed_ids.length
          ? `已处理 ${result.removed} 个会话；${result.failed_ids.length} 个资料清理失败，已停止同步，请在列表中重试。`
          : deleteDocuments
            ? `已处理 ${result.removed} 个会话并删除本地资料。`
            : `已移除 ${result.removed} 个订阅，资料已保留。`,
      );
      setTarget(null);
      await reload();
    } catch (error) {
      setError(`${errorText(error)} 请关闭弹窗，核对最新列表后重试。`);
      await reload();
    } finally {
      lock.current = false;
      setBusy(false);
    }
  }
  /** 保存单项版本；轮询导致该来源变化时由服务端拒绝旧确认。 */
  function confirmOne(source: Source) {
    setError('');
    setTarget({
      selection: { scope: 'one', id: source.id, version: source.version },
      count: 1,
      label: source.label,
      deleteOnly: source.subscribed === false,
    });
  }
  return (
    <section className="subscription-manager" aria-label="订阅管理">
      <div className="subscription-toolbar">
        <div>
          <h2>会话订阅</h2>
          <p>管理同步范围，资料按日期归档。</p>
        </div>
        <div className="subscription-toolbar-actions">
          <button
            className="subscription-add"
            disabled={busy}
            aria-expanded={adding}
            onClick={() => setAdding(!adding)}
          >
            <Plus size={15} />
            添加订阅
          </button>
          <button
            className="subscription-remove-all"
            disabled={busy || !subscriptions.length || !data.subscription_revision}
            onClick={() => {
              setError('');
              setTarget({
                selection: { scope: 'all', revision: data.subscription_revision! },
                count: subscriptions.length,
              });
            }}
          >
            <Trash2 size={14} />
            移除全部订阅
          </button>
        </div>
      </div>
      {data.connection?.auto_subscribe_private && (
        <p className="subscription-archive-note">
          私聊自动订阅 · 群聊手动添加。已暂停或移除的私聊不会被自动恢复，新发现的私聊仍会加入。
          {data.connection.discovery_error && (
            <span role="status">
              {data.connection.discovery_error === 'communication_discovery_limit'
                ? '本轮发现达到扫描上限，将稍后重新扫描；遗漏会话可手动添加。'
                : '私聊自动发现暂时失败，后台会重试，也可手动添加。'}
            </span>
          )}
        </p>
      )}
      {adding && (
        <div className="subscription-picker">
          <SourcePicker sources={data.sources} report={report} onAdded={reload} />
        </div>
      )}
      <div className="subscription-filters">
        <div className="subscription-tabs" role="group" aria-label="订阅状态">
          <button
            aria-pressed={!archived}
            onClick={() => {
              setArchived(false);
              setPage(0);
            }}
          >
            已订阅 <span>{subscriptions.length}</span>
          </button>
          <button
            aria-pressed={archived}
            onClick={() => {
              setArchived(true);
              setPage(0);
            }}
          >
            已保留资料 <span>{retained.length}</span>
          </button>
        </div>
        <label className="subscription-search">
          <Search size={15} />
          <input
            type="search"
            aria-label="搜索订阅会话"
            placeholder="搜索群聊或联系人"
            value={query}
            onChange={(event) => {
              setQuery(event.target.value);
              setPage(0);
            }}
          />
        </label>
      </div>
      {notice && (
        <p className="subscription-notice" role="status">
          {notice}
        </p>
      )}
      {archived && (
        <p className="subscription-archive-note">
          这些会话已移除订阅，保留的资料仍可在沟通记录中浏览。
        </p>
      )}
      <div className="subscription-list">
        <div className="subscription-columns" aria-hidden="true">
          <span>会话</span>
          <span>状态</span>
          <span>已整理 / 总天数</span>
          <span>操作</span>
        </div>
        {visible.map((source) => {
          const stats = progress.get(source.id);
          const open = expanded === source.id;
          return (
            <div className="subscription-item" key={source.id}>
              <div className="subscription-row">
                <button
                  className="subscription-name"
                  aria-expanded={open}
                  onClick={() => setExpanded(open ? null : source.id)}
                  title={source.label}
                >
                  {open ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
                  <span>{source.label}</span>
                </button>
                <span
                  className={`subscription-status ${source.error && source.enabled ? 'failed' : ''}`}
                >
                  <i />
                  {status(source)}
                </span>
                <span className="subscription-progress">
                  {stats?.ready ?? 0}
                  <span> / {stats?.total ?? 0}</span>
                </span>
                <div className="subscription-actions">
                  {source.subscribed === false ? (
                    <button
                      className="subscription-restore"
                      disabled={busy}
                      onClick={() => void change(source, 'restore')}
                    >
                      重新订阅
                    </button>
                  ) : (
                    <>
                      <button
                        className="icon-button"
                        title={source.enabled ? '暂停同步' : '恢复同步'}
                        aria-label={`${source.enabled ? '暂停同步' : '恢复同步'}：${source.label}`}
                        disabled={busy}
                        onClick={() => void change(source, 'toggle')}
                      >
                        {source.enabled ? <Pause size={15} /> : <Play size={15} />}
                      </button>
                      <button
                        className="icon-button"
                        title="同步最新消息"
                        aria-label={`同步最新消息：${source.label}`}
                        disabled={busy || !source.enabled}
                        onClick={() => void change(source, 'sync')}
                      >
                        <RefreshCw size={15} />
                      </button>
                    </>
                  )}
                  <button
                    className="icon-button"
                    title={source.subscribed === false ? '删除保留资料' : '移除订阅'}
                    aria-label={`${source.subscribed === false ? '删除保留资料' : '移除订阅'}：${source.label}`}
                    disabled={busy}
                    onClick={() => confirmOne(source)}
                  >
                    <Trash2 size={15} />
                  </button>
                </div>
              </div>
              {open && (
                <div className="subscription-details">
                  <p>
                    {source.last_synced_at
                      ? `上次同步：${new Date(source.last_synced_at).toLocaleString()}`
                      : '尚未完成首次同步'}
                    {source.error ? ' · 同步失败，请检查授权后重试。' : ''}
                  </p>
                  {stats && (
                    <div>
                      <span>
                        文字资料 {stats.ready}/{stats.total} 天
                      </span>
                      <span>待整理 {stats.summarizing} 天</span>
                      <span>统计更新中 {stats.checking} 天</span>
                      <span>索引处理中 {stats.indexing} 天</span>
                      <span>整理失败 {stats.errors} 天</span>
                      <span>
                        图片解读 {stats.images_ready}/{stats.images}
                        {stats.images_failed ? ` · ${stats.images_failed} 张失败` : ''}
                      </span>
                    </div>
                  )}
                </div>
              )}
            </div>
          );
        })}
        {!visible.length && (
          <p className="subscription-empty">
            {query
              ? '没有匹配的会话。'
              : archived
                ? '暂无移除订阅后保留的资料。'
                : '暂无订阅，点击“添加订阅”选择会话。'}
          </p>
        )}
      </div>
      {filtered.length > SOURCE_PAGE_SIZE && (
        <div className="subscription-pagination">
          <span>
            {filtered.length} 个会话 · 第 {currentPage + 1}/
            {Math.ceil(filtered.length / SOURCE_PAGE_SIZE)} 页
          </span>
          <button disabled={currentPage === 0} onClick={() => setPage(currentPage - 1)}>
            上一页
          </button>
          <button
            disabled={(currentPage + 1) * SOURCE_PAGE_SIZE >= filtered.length}
            onClick={() => setPage(currentPage + 1)}
          >
            下一页
          </button>
        </div>
      )}
      {target && (
        <RemoveSubscriptionDialog
          target={target}
          busy={busy}
          error={error}
          onClose={() => setTarget(null)}
          onConfirm={(deleteDocuments) => void remove(deleteDocuments)}
        />
      )}
    </section>
  );
}
