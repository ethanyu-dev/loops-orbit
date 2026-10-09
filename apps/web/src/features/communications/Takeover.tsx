import { useEffect, useState } from 'react';
import { api, ApiError, errorText } from '../../api';
import './takeover.css';

// 处理原因由服务端枚举映射，不向界面展示供应商响应或凭证。
const REASONS: Record<string, string> = {
  not_matched: '未明确匹配单一问题',
  no_evidence: '没有找到已发布的通用知识',
  unanswerable: '无法形成有依据的完整回答',
  answer_not_supported: '回答未通过证据复核',
  owner_replied: '你已回复',
  conversation_changed: '会话已有新消息或原问题已变化',
  settings_changed: '接管设置已修改',
  rules_changed: '问题文件已变化，旧任务已取消',
  scope_changed: '授权、订阅、资料或消息状态已变化',
  expired: '消息已超出处理时限',
  interrupted: '处理被中断，未自动重试',
  takeover_judge_unavailable: 'Jev 判断暂时不可用',
  takeover_answer_unavailable: '回答模型暂时不可用',
  takeover_timeout: '处理超时',
};
const STATUS: Record<string, string> = {
  queued: '等待判断',
  evaluating: '正在判断',
  dispatching: '正在发送',
  sent: '已回复',
  ignored: '未回复',
  failed: '处理失败，未回复',
  unknown: '发送结果待核对',
};

/** 管理员设置与最近处理记录；不包含用户令牌或模型密钥。 */
interface Snapshot {
  /** 服务端已配置 Jev。 */
  configured: boolean;
  /** 当前用户令牌具有发送权限。 */
  authorized: boolean;
  /** 服务端实际读取的问题文件。 */
  rules_file: string;
  /** 每次保存均以此版本为前提。 */
  settings: {
    /** 显式启用开关。 */
    enabled: boolean;
    /** 服务端验证并绑定的本人自聊；空值表示测试关闭。 */
    self_test_chat_id: string | null;
    /** 允许接管的标准问题。 */
    topics: string[];
    /** 匹配与回答复核的概率下限。 */
    threshold: number;
    /** 并发修改版本。 */
    version: number;
    /** 页面确认的问题文件版本。 */
    rules_revision: string;
    /** 文件缺失或无效时暂停接管。 */
    rules_error: string | null;
  };
  /** 最近五十次处理，无法回答的消息同样可追踪。 */
  jobs: {
    /** 处理记录的稳定标识。 */
    id: string;
    /** 联系人或私聊显示名称。 */
    label: string;
    /** 本次参与判断的原问题，仅管理员可查看。 */
    question: string;
    /** 等待、静默或投递结果。 */
    status: string;
    /** 保持静默或失败的稳定原因。 */
    reason: string | null;
    /** 命中的规则主题。 */
    topic: string | null;
    /** Jev 返回的肯定概率。 */
    probability: number | null;
    /** 通过格式及引文校验的草稿，仅管理员可见，不代表已发送。 */
    draft_answer: string | null;
    /** 回答复核概率，与话题匹配概率独立；未记录时为空。 */
    review_probability: number | null;
    /** 这条任务实际使用的阈值，不随当前设置改变。 */
    decision_threshold: number | null;
    /** 已进入投递阶段的带标识正文。 */
    answer: string | null;
    /** 消息首次入队时间。 */
    created_at: string;
  }[];
}

/** 问题来自文件；页面仅编辑开关和阈值，后台刷新不覆盖未保存内容。 */
export function Takeover({ report }: { report: (e: unknown) => void }) {
  const [data, setData] = useState<Snapshot | null>(null);
  const [enabled, setEnabled] = useState(false);
  const [selfTest, setSelfTest] = useState(false);
  const [rulesRevision, setRulesRevision] = useState('');
  const [threshold, setThreshold] = useState('0.9');
  const [version, setVersion] = useState(0);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [saved, setSaved] = useState(false);
  /** 显式载入编辑版本，避免后台刷新覆盖未保存内容。 */
  async function load() {
    try {
      const next = await api<Snapshot>('/communications/takeover');
      setData(next);
      setEnabled(next.settings.enabled);
      setSelfTest(!!next.settings.self_test_chat_id);
      setRulesRevision(next.settings.rules_revision);
      setThreshold(String(next.settings.threshold));
      setVersion(next.settings.version);
      setError('');
    } catch (e) {
      setError(errorText(e));
    }
  }
  useEffect(() => {
    void load();
    let active = true;
    const timer = window.setInterval(() => {
      void api<Snapshot>('/communications/takeover')
        .then((next) => {
          if (active) setData(next);
        })
        .catch(() => {
          /* 保留上次记录，显式刷新会展示错误。 */
        });
    }, 5000);
    return () => {
      active = false;
      window.clearInterval(timer);
    };
  }, []);
  /** 保存成功后采用服务端新版本；开启仅从保存后的新消息开始。 */
  async function save(event: React.FormEvent) {
    event.preventDefault();
    setBusy(true);
    setSaved(false);
    try {
      await api('/communications/takeover', {
        method: 'PUT',
        body: JSON.stringify({
          enabled,
          self_test_enabled: enabled && selfTest,
          rules_revision: rulesRevision,
          threshold: Number(threshold),
          version,
        }),
      });
      await load();
      setSaved(true);
    } catch (e) {
      report(e);
    } finally {
      setBusy(false);
    }
  }
  if (!data)
    return (
      <section className="settings-card">
        <p role={error ? 'alert' : 'status'}>{error || '正在读取接管设置…'}</p>
        {error && <button onClick={() => void load()}>重试</button>}
      </section>
    );
  return (
    <div className="takeover-workspace">
      <form
        className="settings-card takeover-form"
        onSubmit={(event) => void save(event)}
        onChange={() => setSaved(false)}
      >
        <h2>特定问题自动接管</h2>
        <p>
          仅处理已订阅私聊的新文字消息。Jev
          判断是否属于你允许的问题，现有模型仅使用你已发布的通用知识回答，不读取私人记忆或其他会话原文。无法完整回答时保持静默，每条回复固定带有{' '}
          <strong>[Agent 自动回复]</strong> 标识。
        </p>
        <p>
          接管期间以约 15
          秒一轮为轮询目标，实际延迟受会话数量及接口耗时影响。每条新消息重新判断；你已回复或对话发生变化时停止旧答案。超过
          5 分钟的消息不再自动补发。
        </p>
        {!data.configured && (
          <p role="status">尚未配置 Jev 服务，请在服务端设置 TYPESAFE_API_KEY。</p>
        )}
        {!data.authorized && (
          <p role="status">尚未确认用户发送授权，请使用上方“重新授权”连接飞书。</p>
        )}
        {error && <p role="alert">{error}</p>}
        <label className="takeover-toggle">
          <input
            type="checkbox"
            checked={enabled}
            disabled={
              busy ||
              ((!data.configured || !data.authorized || !!data.settings.rules_error) && !enabled)
            }
            onChange={(e) => setEnabled(e.target.checked)}
          />
          允许 agent 以我的身份回复匹配的问题
        </label>
        <label className="takeover-toggle">
          <input
            type="checkbox"
            checked={selfTest}
            disabled={busy || !enabled}
            onChange={(e) => setSelfTest(e.target.checked)}
          />
          自聊测试：将我发给自己的问题按他人询问处理
        </label>
        <p>
          首次开启或关闭后重新开启并保存，会以你的身份向自己发送一条测试提示，并订阅这段自聊的新消息。
          保存成功后，在飞书打开自己的聊天，发送下方允许的问题即可测试。仅使用已发布的通用知识，保留同样的话题判断、回答复核和回复频率限制。
          你发给其他联系人的消息不会触发测试；关闭测试不会删除已采集资料。
        </p>
        <section className="takeover-rules" aria-label="允许接管的问题">
          <h3>允许接管的问题</h3>
          <p>
            在文件 <code>{data.rules_file}</code> 中维护，最多 20
            条。文件变更后自动读取，旧任务会取消。
          </p>
          {data.settings.rules_error ? (
            <p role="alert">
              {errorText(new ApiError(409, data.settings.rules_error))}{' '}
              接管已暂停，修复文件后自动恢复，只处理新的消息。
            </p>
          ) : (
            <ol>
              {data.settings.topics.map((topic) => (
                <li key={topic}>{topic}</li>
              ))}
            </ol>
          )}
          {(version !== data.settings.version ||
            rulesRevision !== data.settings.rules_revision) && (
            <p role="status">问题或设置已变化，请重新载入后再保存。</p>
          )}
        </section>
        <label>
          接管及回答复核阈值
          <input
            type="number"
            min="0.5"
            max="1"
            step="0.01"
            value={threshold}
            onChange={(e) => setThreshold(e.target.value)}
            required
          />
        </label>
        <p>
          阈值是 Jev 对判断为真的概率估计。默认 0.90；应结合实际命中记录调整，不能视为准确率保证。
        </p>
        <div className="takeover-actions">
          <button className="primary-button" disabled={busy}>
            {busy ? '正在保存…' : '保存设置'}
          </button>
          <button type="button" disabled={busy} onClick={() => void load()}>
            重新载入
          </button>
          {saved && <span role="status">已保存</span>}
        </div>
      </form>
      <section className="settings-card">
        <h2>最近处理记录</h2>
        {data.jobs.length === 0 ? (
          <p>还没有处理记录。开启后只处理新的私聊问题。</p>
        ) : (
          <ul className="takeover-jobs">
            {data.jobs.map((job) => (
              <li key={job.id}>
                <div>
                  <strong>{job.label}</strong>
                  <span>{STATUS[job.status] || job.status}</span>
                </div>
                <small>
                  {new Date(job.created_at).toLocaleString()}
                  {job.topic ? ` · ${job.topic}` : ''}
                  {job.probability !== null
                    ? ` · 匹配概率 ${(job.probability * 100).toFixed(1)}%`
                    : ''}
                </small>
                <p className="takeover-answer">{job.question}</p>
                {job.decision_threshold != null && (
                  <p>
                    回答复核：
                    {job.review_probability != null
                      ? `${(job.review_probability * 100).toFixed(1)}%`
                      : '未取得分数'}
                    {' · '}本次阈值 {(job.decision_threshold * 100).toFixed(1)}%
                  </p>
                )}
                {job.reason && <p>{REASONS[job.reason] || '处理未完成，请检查连接与服务配置。'}</p>}
                {job.draft_answer && (
                  <details>
                    <summary>查看生成草稿（不代表已发送）</summary>
                    <p className="takeover-answer">{job.draft_answer}</p>
                  </details>
                )}
                {job.reason === 'answer_not_supported' && (
                  <p>复核分数未达到本次阈值；模型未提供具体拒绝理由。</p>
                )}
                {job.decision_threshold == null && job.reason === 'answer_not_supported' && (
                  <p>历史记录未保存草稿和复核分数，无法还原。</p>
                )}
                {job.answer && (
                  <div>
                    <strong>
                      {job.status === 'sent' ? '已发送正文' : '投递正文（请结合发送状态核对）'}
                    </strong>
                    <p className="takeover-answer">{job.answer}</p>
                  </div>
                )}
                {job.status === 'unknown' && <p>请在飞书核对是否已发出；系统不会自动重发。</p>}
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}
