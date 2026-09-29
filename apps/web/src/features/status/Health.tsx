import { useEffect, useState } from 'react';
import { Activity, ArrowDown, Check, CircleHelp, Globe2, MessageSquare } from 'lucide-react';
import { api } from '../../api';
import type { Status } from '../../types';
import { Empty, Spinner } from '../../components/Feedback';

// 状态页低频刷新全局快照，避免与聊天任务轮询混用。
const POLL_INTERVAL_MS = 10000;
// 任务卡片顺序与服务端统计字段显式对应。
const METRICS = [
  { key: 'completed', title: '已完成', icon: Check },
  { key: 'running', title: '执行中', icon: Activity },
  { key: 'queued', title: '等待中', icon: ArrowDown },
  { key: 'failed', title: '失败', icon: CircleHelp },
] as const;

/** 状态来自数据库快照，不用静态徽章模拟真实服务健康度。 */
export function Health({ report }: { report: (e: unknown) => void }) {
  const [status, setStatus] = useState<Status | null>(null);
  const [updated, setUpdated] = useState('');
  useEffect(() => {
    let active = true;
    const load = async () => {
      try {
        const data = await api<Status>('/admin/status');
        if (active) {
          setStatus(data);
          setUpdated(new Date().toLocaleTimeString('zh-CN'));
        }
      } catch (e) {
        if (active) report(e);
      }
    };
    void load();
    const interval = setInterval(() => void load(), POLL_INTERVAL_MS);
    return () => {
      active = false;
      clearInterval(interval);
    };
  }, [report]);
  return (
    <div className="settings-page">
      <div className="page-eyebrow">SYSTEM OBSERVABILITY</div>
      <h1>每一次运行，都有迹可循。</h1>
      <p className="page-description">查看任务队列与入口配置。数据每 10 秒自动更新。</p>
      {!status ? (
        <Empty>
          <Spinner />
        </Empty>
      ) : (
        <>
          <div className="metrics-grid">
            {METRICS.map((item) => (
              <div className={`metric-card ${item.key}`} key={item.key}>
                <div>
                  {item.title}
                  <item.icon size={17} />
                </div>
                <strong>{status.runs[item.key]}</strong>
                <small>持久化任务 · 累计</small>
              </div>
            ))}
          </div>
          <div className="status-layout">
            <section className="settings-card">
              <div className="card-title">
                <Activity size={18} />
                <h2>运行配置</h2>
              </div>
              <dl className="status-list">
                <div>
                  <dt>Agent runtime</dt>
                  <dd>Orbit / 自研</dd>
                </div>
                <div>
                  <dt>当前模型</dt>
                  <dd>{status.model}</dd>
                </div>
                <div>
                  <dt>并发 worker / 实例</dt>
                  <dd>{status.workers_per_instance}</dd>
                </div>
                <div>
                  <dt>数据持久化</dt>
                  <dd>PostgreSQL</dd>
                </div>
                <div>
                  <dt>版本</dt>
                  <dd>{status.version}</dd>
                </div>
              </dl>
            </section>
            <section className="settings-card">
              <div className="card-title">
                <Globe2 size={18} />
                <h2>服务入口</h2>
              </div>
              <div className="channel-row">
                <div className="channel-icon">
                  <Globe2 size={19} />
                </div>
                <div>
                  <strong>网页</strong>
                  <small>管理员与临时访客</small>
                </div>
                <span className="tag good">已连接</span>
              </div>
              <div className="channel-row">
                <div className="channel-icon">
                  <MessageSquare size={19} />
                </div>
                <div>
                  <strong>飞书</strong>
                  <small>
                    {status.feishu_enabled ? '白名单用户 · 单聊文本' : '通过环境变量配置应用凭据'}
                  </small>
                </div>
                <span className="tag">{status.feishu_enabled ? '已配置' : '未配置'}</span>
              </div>
              <div className="delivery-summary">
                飞书回复：{status.delivery.completed} 已发送 ·{' '}
                {status.delivery.queued + status.delivery.running} 待发送 · {status.delivery.failed}{' '}
                失败
              </div>
            </section>
          </div>
          <p className="status-footnote">
            <span className="status-dot" />
            最近更新于 {updated}
            <span>模型与飞书的实际连通性以任务结果为准</span>
          </p>
        </>
      )}
    </div>
  );
}
