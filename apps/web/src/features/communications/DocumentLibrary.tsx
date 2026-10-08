import { useEffect, useState, type FormEvent } from 'react';
import { Link, useSearchParams } from 'react-router-dom';
import { ChevronRight, FileText, Folder, FolderOpen, RefreshCw, Search, X } from 'lucide-react';
import { Spinner } from '../../components/Feedback';
import { DirectoryTree } from './DirectoryTree';
import { fileStatus, groupFiles, readOffset, type FilePage, type LibraryFile } from './library';
import { useLibraryDays, useLibraryRequest } from './useLibrary';
import type { Document, Source } from './types';
import './library.css';

// 与文件接口的固定分页大小一致；内容检索沿用独立接口的相关性数量上限。
const FILE_PAGE_SIZE = 50;

/** 内容命中使用检索接口返回的来源名称，不依赖最近文档快照。 */
interface SearchHit {
  /** 现有详情路由使用的文档。 */
  document: Document;
  /** 检索时读取的来源名称。 */
  label: string;
  /** 命中的相关原文。 */
  messages: { text: string }[];
}

/** 文件管理视图把日期导航、完整历史搜索和详情地址组合起来，浏览状态保存在 URL。 */
export function DocumentLibrary({
  sources,
}: {
  /** 已订阅来源用于显示内容检索结果的暂停状态。 */
  sources: Source[];
}) {
  const [params, setParams] = useSearchParams();
  const query = params.get('q') ?? '';
  const mode = params.get('mode') === 'content' ? 'content' : 'files';
  const offset = readOffset(params.get('offset'));
  const [draft, setDraft] = useState(query);
  const [searchMode, setSearchMode] = useState(mode);
  const [revision, setRevision] = useState(0);
  const directory = useLibraryDays(revision);
  const requestedDay = params.get('day');
  const day =
    requestedDay === 'all' ? '' : (requestedDay ?? (query ? '' : (directory.days[0]?.day ?? '')));
  const contentSearch = mode === 'content' && !!query;
  useEffect(() => {
    setDraft(query);
    setSearchMode(mode);
  }, [query, mode]);
  const fileParams = new URLSearchParams({ offset: String(offset) });
  if (day) fileParams.set('day', day);
  if (query) fileParams.set('q', query);
  const ready =
    !!requestedDay ||
    !!query ||
    directory.days.length > 0 ||
    (!directory.loading && !directory.error);
  const path = !ready
    ? null
    : contentSearch
      ? `/communications/search?q=${encodeURIComponent(query)}`
      : `/communications/library/files?${fileParams}`;
  const result = useLibraryRequest<FilePage | SearchHit[]>(path, revision);
  const files: LibraryFile[] = Array.isArray(result.data)
    ? result.data.map((hit) => ({
        ...hit.document,
        source_label: hit.label,
        source_enabled:
          sources.find((source) => source.id === hit.document.source_id)?.enabled ?? true,
        excerpt: hit.messages.map((message) => message.text).join(' · '),
      }))
    : (result.data?.items ?? []);
  const total = Array.isArray(result.data) ? result.data.length : (result.data?.total ?? 0);
  const next = !Array.isArray(result.data) ? result.data?.next_offset : null;
  const groups = groupFiles(files);
  const loading = result.loading || (!ready && directory.loading);
  const loadError = result.error || (!ready ? directory.error : '');
  const title = query ? `“${query}”的搜索结果` : day || '全部资料';

  /** 搜索始终从全部日期的第一页开始，避免旧目录限制让用户误以为没有匹配。 */
  function search(event: FormEvent) {
    event.preventDefault();
    const next = new URLSearchParams();
    if (draft.trim()) {
      next.set('q', draft.trim());
      next.set('mode', searchMode);
    } else next.set('day', 'all');
    setParams(next);
    setRevision((value) => value + 1);
  }
  /** 切换目录清空旧搜索与分页，链接可复制并支持浏览器返回。 */
  function openDay(value: string) {
    setParams({ day: value });
  }
  /** 翻页保留已解析的日期，刷新后不会因新增资料自动跳到另一天。 */
  function openPage(value: number) {
    const next = new URLSearchParams(params);
    next.set('day', day || 'all');
    next.set('offset', String(value));
    setParams(next);
  }
  /** 详情地址携带浏览状态，详情页的返回按钮能恢复搜索和文件分页。 */
  function documentPath(id: string) {
    const state = new URLSearchParams(params);
    if (!query) state.set('day', day || 'all');
    return `/communications/records/${id}?${state}`;
  }

  return (
    <section className="document-library" aria-label="沟通资料文件管理">
      <div className="library-toolbar">
        <form className="library-search" onSubmit={search}>
          <Search size={17} aria-hidden="true" />
          <input
            aria-label="搜索沟通资料"
            value={draft}
            maxLength={2000}
            onChange={(event) => setDraft(event.target.value)}
            placeholder={
              searchMode === 'files' ? '搜索群聊、联系人或日期…' : '搜索消息内容，或问一个问题…'
            }
          />
          {draft && (
            <button
              type="button"
              className="icon-button"
              aria-label="清空搜索"
              onClick={() => {
                setDraft('');
                openDay('all');
              }}
            >
              <X size={15} />
            </button>
          )}
          <select
            aria-label="搜索范围"
            value={searchMode}
            onChange={(event) => setSearchMode(event.target.value as 'files' | 'content')}
          >
            <option value="files">名称 / 日期</option>
            <option value="content">消息内容</option>
          </select>
          <button className="library-search-submit" type="submit">
            搜索
          </button>
        </form>
        <button
          className="library-refresh"
          aria-label="刷新资料目录"
          title="刷新资料目录"
          disabled={loading || directory.loading}
          onClick={() => setRevision((value) => value + 1)}
        >
          <RefreshCw size={16} />
          <span>刷新</span>
        </button>
      </div>
      <div className="library-layout">
        <DirectoryTree
          days={directory.days}
          selected={query ? '' : day}
          onSelect={openDay}
          more={directory.more}
          loading={directory.loading}
          error={directory.error}
          onMore={directory.loadMore}
          onRetry={() => setRevision((value) => value + 1)}
        />
        <div className="library-content">
          <nav className="library-breadcrumb" aria-label="文件路径">
            <button onClick={() => openDay('all')}>全部资料</button>
            <ChevronRight size={13} />
            <span>
              {query ? '搜索结果' : day ? `${day.slice(0, 7).replace('-', ' 年 ')} 月` : '所有日期'}
            </span>
            {!query && day && (
              <>
                <ChevronRight size={13} />
                <span>{day.slice(8)} 日</span>
              </>
            )}
          </nav>
          <div className="library-heading">
            <h2>{title}</h2>
            {result.data && (
              <span>
                {total} 份{query ? '匹配资料' : '资料'}
              </span>
            )}
          </div>
          <p className="library-description">
            {contentSearch
              ? '按相关性展示最多 6 份资料；暂停同步的会话不参与内容检索。'
              : '每个群聊或联系人按日归档，打开文件查看整理结果与原始消息。'}
          </p>
          {loadError ? (
            <div className="library-empty" role="alert">
              <p>{loadError}</p>
              <button onClick={() => setRevision((value) => value + 1)}>重新加载文件</button>
            </div>
          ) : loading ? (
            <div className="library-empty" role="status">
              <Spinner />
              <p>{contentSearch ? '正在检索消息内容…' : '正在读取文件…'}</p>
            </div>
          ) : !files.length ? (
            <div className="library-empty">
              <FolderOpen size={34} />
              <h3>{query ? '没有找到匹配资料' : '这个目录还没有资料'}</h3>
              <p>
                {query
                  ? '试试其他关键词，或切换搜索范围。'
                  : '订阅会话并导入历史消息后，文件会出现在对应日期下。'}
              </p>
              {query ? (
                <button onClick={() => openDay('all')}>返回全部资料</button>
              ) : (
                <Link to="/communications/sync">前往历史导入</Link>
              )}
            </div>
          ) : (
            <div className="library-files">
              {groups.map((group) => (
                <section
                  className="library-file-group"
                  key={group.day}
                  aria-label={`${group.day} 的资料`}
                >
                  {(!day || query) && (
                    <button className="library-file-date" onClick={() => openDay(group.day)}>
                      <Folder size={15} />
                      {group.day}
                      <ChevronRight size={13} />
                    </button>
                  )}
                  <div className="library-column-labels" aria-hidden="true">
                    <span>文件名称</span>
                    <span>整理状态</span>
                    <span />
                  </div>
                  {group.files.map((file) => {
                    const status = fileStatus(file);
                    return (
                      <Link className="library-file" to={documentPath(file.id)} key={file.id}>
                        <span className="library-file-main">
                          <span className="library-file-icon">
                            <FileText size={21} />
                          </span>
                          <span className="library-file-copy">
                            <strong>{file.source_label}</strong>
                            <small>
                              {file.day} · 沟通记录
                              {file.source_subscribed === false
                                ? ' · 已移除订阅'
                                : !file.source_enabled
                                  ? ' · 已暂停同步'
                                  : ''}
                            </small>
                            {file.excerpt && (
                              <span className="library-excerpt">{file.excerpt}</span>
                            )}
                          </span>
                        </span>
                        <span className={`library-file-status ${status.kind}`}>
                          <i />
                          {status.label}
                        </span>
                        <ChevronRight size={15} className="library-file-arrow" />
                      </Link>
                    );
                  })}
                </section>
              ))}
            </div>
          )}
          {!contentSearch && !loading && !loadError && (offset > 0 || next != null) && (
            <div className="library-pagination">
              <button
                disabled={offset === 0}
                onClick={() => openPage(Math.max(0, offset - FILE_PAGE_SIZE))}
              >
                上一页
              </button>
              <span>
                第 {Math.floor(offset / FILE_PAGE_SIZE) + 1} 页 · 共 {total} 份
              </span>
              <button
                disabled={next == null}
                onClick={() => {
                  if (next != null) openPage(next);
                }}
              >
                下一页
              </button>
            </div>
          )}
        </div>
      </div>
    </section>
  );
}
