import { ChevronDown, ExternalLink, KeyRound } from 'lucide-react';

/** 配置细节按需展开；只有缺少配置时默认展开，已连接用户优先看到使用入口。 */
export function ConnectionHelp({ needsSetup }: { needsSetup: boolean }) {
  return (
    <details className="integration-help" open={needsSetup || undefined}>
      <summary>
        <KeyRound size={18} aria-hidden="true" />
        <span>
          配置与使用帮助<small>获取密钥、更换账号与权限说明</small>
        </span>
        <ChevronDown size={18} className="integration-chevron" aria-hidden="true" />
      </summary>
      <div className="integration-help-content">
        <ol>
          <li>
            <strong>创建个人 API Key</strong>
            <p>在 Linear 的个人安全设置中创建密钥，选择 Read、Write 权限及需要访问的团队。</p>
            <a href="https://linear.app/settings/account/security" target="_blank" rel="noreferrer">
              打开 Linear 安全设置
              <ExternalLink size={13} aria-hidden="true" />
            </a>
          </li>
          <li>
            <strong>配置服务端并重启</strong>
            <p>
              将密钥填入服务端环境变量 <code>LINEAR_API_KEY</code>。可选设置{' '}
              <code>LINEAR_WORKSPACE_SLUG</code> 限定工作空间。
            </p>
          </li>
          <li>
            <strong>返回此页验证连接</strong>
            <p>
              点击「验证并连接」，核对显示的账号和工作空间。更换密钥后，也需要重启服务并重新验证。
            </p>
          </li>
        </ol>
        <div className="integration-help-note">
          无需 Linear 管理员角色；如果没有创建密钥的入口，请工作空间管理员开启成员 API Key 功能。
          身份验证不会试写事项，实际读写权限由 Linear
          判断。已绑定为本人且在允许名单中的飞书账号可使用此连接；访客和其他飞书账号无权使用，解绑后停止授权。
        </div>
      </div>
    </details>
  );
}
