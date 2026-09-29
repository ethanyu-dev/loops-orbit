import { useState, type FormEvent } from 'react';
import { ArrowRight, KeyRound, ShieldCheck } from 'lucide-react';
import { api } from '../../api';
import { OrbitMark } from '../../components/OrbitMark';
import { ThemeToggle } from '../theme/ThemeToggle';
import { Spinner } from '../../components/Feedback';

/** 登录表单只在提交过程中持有根 token，成功后立即清空输入。 */
export function Login({
  error,
  onLogin,
  report,
}: {
  /** 应用统一维护的登录或连接错误。 */
  error: string;
  /** 登录成功后重新加载服务端身份。 */
  onLogin: () => Promise<void>;
  /** 将认证失败交给应用统一处理。 */
  report: (e: unknown) => void;
}) {
  const [token, setToken] = useState('');
  const [busy, setBusy] = useState(false);
  /** 登录成功后交由应用重新获取身份，不持久化根密钥。 */
  async function submit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    try {
      await api('/auth/login', { method: 'POST', body: JSON.stringify({ token }) });
      setToken('');
      await onLogin();
    } catch (e) {
      report(e);
    } finally {
      setBusy(false);
    }
  }
  return (
    <div className="login-page">
      <div className="login-theme">
        <ThemeToggle />
      </div>
      <div className="login-brand">
        <OrbitMark small />
        <span>orbit.</span>
      </div>
      <div className="login-card">
        <div className="eyebrow">
          <span /> A SPACE FOR YOUR MIND
        </div>
        <OrbitMark />
        <h1>你好，欢迎回来。</h1>
        <p>
          你的想法，你的节奏。
          <br />
          进入专属于你的 Agent 工作空间。
        </p>
        <form onSubmit={submit}>
          <label htmlFor="admin-token">ADMIN TOKEN</label>
          <div className="token-input">
            <KeyRound size={17} />
            <input
              id="admin-token"
              type="password"
              autoComplete="current-password"
              autoFocus
              placeholder="输入你的管理员密钥"
              value={token}
              onChange={(e) => setToken(e.target.value)}
              required
            />
          </div>
          {error && (
            <div className="login-error" role="alert">
              {error}
            </div>
          )}
          <button className="primary-button" disabled={busy || !token.trim()}>
            {busy ? (
              <Spinner />
            ) : (
              <>
                进入工作空间
                <ArrowRight size={17} />
              </>
            )}
          </button>
        </form>
        <div className="login-note">
          <ShieldCheck size={14} />
          通过临时链接访问？直接打开管理员分享的链接即可。
        </div>
      </div>
      <footer>
        ORBIT <span>个人智能，自在运行。</span>
      </footer>
    </div>
  );
}
