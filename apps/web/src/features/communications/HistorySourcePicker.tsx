import { useState } from 'react';
import type { Source } from './types';

/** 搜索仅缩小可见范围，跨搜索保留勾选；全选搜索结果不影响已选的其他会话。 */
export function HistorySourcePicker({
  sources,
  selected,
  onChange,
}: {
  sources: Source[];
  selected: Set<string>;
  onChange: (ids: Set<string>) => void;
}) {
  const [query, setQuery] = useState('');
  const visible = sources.filter((source) =>
    source.label.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase()),
  );
  return (
    <div className="history-source-picker">
      <label>
        会话 · 已选 {selected.size} / {sources.length}
        <input
          type="search"
          placeholder="搜索会话名称…"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
        />
      </label>
      <div className="history-selection-actions">
        <button
          type="button"
          disabled={!visible.length}
          onClick={() => onChange(new Set([...selected, ...visible.map((s) => s.id)]))}
        >
          {query.trim() ? `全选搜索结果（${visible.length}）` : '全选会话'}
        </button>
        <button type="button" disabled={!selected.size} onClick={() => onChange(new Set())}>
          清空选择
        </button>
      </div>
      <div className="history-source-options" role="group" aria-label="选择要整理的会话">
        {visible.map((source) => (
          <label key={source.id} className="history-source-option">
            <input
              type="checkbox"
              checked={selected.has(source.id)}
              onChange={(event) => {
                const next = new Set(selected);
                if (event.target.checked) next.add(source.id);
                else next.delete(source.id);
                onChange(next);
              }}
            />
            <span>{source.label}</span>
          </label>
        ))}
        {!visible.length && <p>{sources.length ? '没有匹配的会话。' : '暂无可整理的会话。'}</p>}
      </div>
    </div>
  );
}
