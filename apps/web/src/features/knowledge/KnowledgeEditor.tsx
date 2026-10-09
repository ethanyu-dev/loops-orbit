import { useState, type FormEvent } from 'react';
import type { KnowledgeEntry } from './types';
import { api } from '../../api';
import { Spinner } from '../../components/Feedback';

/** 审阅正文与证据分区显示；保存草稿永远不隐式发布。 */
export function KnowledgeEditor({
  entry,
  busy,
  save,
  close,
}: {
  /** 空值表示本人手工补充知识。 */
  entry: KnowledgeEntry | null;
  /** 避免重复提交和关闭在途表单。 */
  busy: boolean;
  /** 状态随当前编辑正文一起提交。 */
  save: (title: string, content: string, publish: boolean, tags: string[]) => Promise<void>;
  /** 退出时丢弃未保存编辑。 */
  close: () => void;
}) {
  const [title, setTitle] = useState(entry?.title ?? '');
  const [content, setContent] = useState(entry?.content ?? '');
  const [tags, setTags] = useState(entry?.tags?.join('，') ?? '');
  const [snapshot, setSnapshot] = useState<string | null>(null);
  const [approved, setApproved] = useState(false);
  /** 发布必须针对当前正文确认；修改正文会清除之前的勾选。 */
  function submit(event: FormEvent) {
    event.preventDefault();
    void save(
      title,
      content,
      approved,
      tags
        .split(/[,，]/)
        .map((tag) => tag.trim())
        .filter(Boolean),
    );
  }
  return (
    <section className="settings-card knowledge-editor">
      <h2>{entry ? '审阅知识' : '手工补充知识'}</h2>
      <form onSubmit={submit}>
        <label htmlFor="knowledge-title">知识主题</label>
        <input
          id="knowledge-title"
          value={title}
          maxLength={120}
          required
          disabled={busy}
          onChange={(event) => {
            setTitle(event.target.value);
            setApproved(false);
          }}
        />
        <label htmlFor="knowledge-content">可对外复用的答案</label>
        <textarea
          id="knowledge-content"
          value={content}
          maxLength={2000}
          rows={8}
          required
          disabled={busy}
          onChange={(event) => {
            setContent(event.target.value);
            setApproved(false);
          }}
        />
        <label htmlFor="knowledge-tags">主题标签（最多 8 个，以逗号分隔）</label>
        <input
          id="knowledge-tags"
          value={tags}
          maxLength={263}
          disabled={busy}
          onChange={(event) => {
            setTags(event.target.value);
            setApproved(false);
          }}
        />
        {entry?.evidence.length ? (
          <div className="knowledge-evidence">
            <h3>证据摘录 · 仅你可见</h3>
            {entry.evidence.map((item, index) => (
              <div key={index}>
                <small>
                  {item.source_day} {item.source_label}
                </small>
                <blockquote>{item.quote}</blockquote>
                {item.snapshot_id && (
                  <button
                    type="button"
                    className="secondary-button"
                    onClick={() => {
                      setSnapshot('正在读取原始快照…');
                      void api<{ messages: { text: string }[] }>(
                        `/knowledge/snapshots/${item.snapshot_id}`,
                      )
                        .then((data) =>
                          setSnapshot(
                            data.messages
                              .map((message) => message.text)
                              .filter(Boolean)
                              .join('\n\n'),
                          ),
                        )
                        .catch(() => setSnapshot('快照暂时无法读取，请重试。'));
                    }}
                  >
                    查看原始快照
                  </button>
                )}
              </div>
            ))}
            <p className="field-note">
              原话存在不代表结论正确。请核对适用范围、时效，以及正文和文档链接是否适合向他人提供。
            </p>
          </div>
        ) : (
          <p className="field-note">手工知识没有自动提取的证据，请确认内容准确、适合对外使用。</p>
        )}
        {snapshot !== null && (
          <div className="knowledge-evidence">
            <button type="button" className="secondary-button" onClick={() => setSnapshot(null)}>
              收起原始快照
            </button>
            <pre style={{ whiteSpace: 'pre-wrap', maxHeight: 320, overflow: 'auto' }}>
              {snapshot}
            </pre>
          </div>
        )}
        <label className="knowledge-approval">
          <input
            type="checkbox"
            checked={approved}
            disabled={busy}
            onChange={(event) => setApproved(event.target.checked)}
          />
          我已核对正文，允许用于他人问答及代我回复。
        </label>
        <p className="field-note">
          仅发布上方答案，原文证据不会提供给对方。发布内容变更或撤回会使旧的问答上下文失效。
        </p>
        <div className="memory-actions">
          <button className="primary-button" disabled={busy}>
            {busy && <Spinner />}
            {approved ? '确认发布' : '保存为候选'}
          </button>
          <button type="button" className="secondary-button" disabled={busy} onClick={close}>
            取消
          </button>
        </div>
      </form>
    </section>
  );
}
