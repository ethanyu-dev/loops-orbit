import { useState } from 'react';
import { ChevronDown, ChevronRight, Folder, FolderOpen, Library } from 'lucide-react';
import { groupMonths, type DayFolder } from './library';

/** 月份与日期组成可折叠目录；普通按钮保留原生 Tab 和 Enter 行为。 */
export function DirectoryTree({
  days,
  selected,
  onSelect,
  more,
  loading,
  error,
  onMore,
  onRetry,
}: {
  /** 已分页读取的日期。 */
  days: DayFolder[];
  /** 空值表示跨日期浏览或搜索。 */
  selected: string;
  /** 打开日期或所有文件。 */
  onSelect: (day: string) => void;
  /** 是否还有较早目录。 */
  more: boolean;
  /** 目录请求中。 */
  loading: boolean;
  /** 目录错误独立于文件区。 */
  error: string;
  /** 追加下一批日期。 */
  onMore: () => void;
  /** 重新读取目录。 */
  onRetry: () => void;
}) {
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const months = groupMonths(days);
  return (
    <nav className="library-tree" aria-label="资料日期目录">
      <div className="library-tree-title">
        资料目录<span>北京时间</span>
      </div>
      <button
        className={`library-root ${!selected ? 'selected' : ''}`}
        onClick={() => onSelect('all')}
        aria-current={!selected ? 'page' : undefined}
      >
        <Library size={16} />
        全部资料
      </button>
      {months.map(({ month, days }, index) => {
        const open = expanded[month] ?? (selected ? selected.startsWith(month) : index === 0);
        return (
          <div className="library-month" key={month}>
            <button
              className="library-month-button"
              aria-expanded={open}
              onClick={() => setExpanded({ ...expanded, [month]: !open })}
            >
              {open ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
              <Folder size={16} />
              {month.replace('-', ' 年 ')} 月
            </button>
            {open && (
              <div className="library-days">
                {days.map((folder) => (
                  <button
                    key={folder.day}
                    className={selected === folder.day ? 'selected' : ''}
                    aria-current={selected === folder.day ? 'page' : undefined}
                    onClick={() => onSelect(folder.day)}
                  >
                    {selected === folder.day ? <FolderOpen size={15} /> : <Folder size={15} />}
                    <span>{folder.day.slice(5).replace('-', ' 月 ')} 日</span>
                    <small>{folder.count}</small>
                  </button>
                ))}
              </div>
            )}
          </div>
        );
      })}
      {error && (
        <div className="library-tree-error" role="alert">
          <p>{error}</p>
          <button onClick={onRetry}>重新加载目录</button>
        </div>
      )}
      {loading && (
        <p className="library-tree-note" role="status">
          正在读取目录…
        </p>
      )}
      {!loading && !error && days.length === 0 && <p className="library-tree-note">暂无日期目录</p>}
      {more && !loading && (
        <button className="library-load-more" onClick={onMore}>
          加载更早日期
        </button>
      )}
    </nav>
  );
}
