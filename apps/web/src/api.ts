import { apiUrl } from './config';

// 服务端业务错误到界面提示的映射，不暴露上游原始响应。
const ERRORS: Record<string, string> = {
  linear_disabled: '服务端尚未配置 Linear 应用。',
  linear_oauth_invalid: '授权回调已失效，请在当前浏览器重新连接。',
  linear_reauthorize: 'Linear 授权已失效，请重新连接。',
  linear_rate_limited: 'Linear 请求较频繁，请稍后再试。',
  linear_unavailable: 'Linear 暂时不可用，请稍后重试。',

  invalid_communication_library: '请检查目录日期或搜索词，关键词最多 2,000 字。',
  communication_disabled: '服务端尚未启用飞书沟通采集。',
  communication_not_connected: '请先连接你的飞书账号。',
  communication_reauthorize: '飞书授权已失效，请重新连接。',
  communication_summary_source_inactive: '来源已暂停、正在删除或尚未完成核对，暂时不能重新整理。',
  communication_summary_retry_unavailable: '当前状态不需要重新整理，请刷新查看结果。',
  communication_source_changed: '资料已被更新、暂停或遗忘，请重新打开后确认。',
  invalid_history_range: '请选择有效日期范围，单次最多一年，开始日期不能晚于今天。',
  invalid_communication_source: '请选择有效的飞书会话和显示名称。',
  invalid_communication_selection: '请选择 1 至 1,000 个不同的历史会话。',
  communication_unavailable: '沟通资料暂时不可用，请检查同步状态或稍后重试。',
  communication_provider_rejected: '飞书拒绝了请求，请检查用户授权及应用消息读取权限。',
  communication_oauth_invalid: '授权回调已失效，请从此浏览器重新发起连接。',
  communication_forbidden: '此身份不能访问这份沟通资料。',
  communication_not_found: '这份沟通资料已不存在。',
  ambiguous_followup_time: '这个时区下的时间不存在或有夏令时歧义，请选择另一个时间。',
  followup_idempotency_conflict: '这次创建操作的内容已改变，请修改表单后重新保存。',
  invalid_followup: '请填写有效事项，最多 500 字。',
  invalid_followup_time: '请选择未来一年内的有效时间；夏令时重复时刻请在对话中指定时区偏移。',
  invalid_followup_preferences: '请检查时区、静默时间和回访间隔。',
  followup_version_conflict: '事项或偏好已经发生变化，请刷新后重试。',
  followup_not_found: '事项不存在或属于其他身份。',
  checkins_disabled: '请先开启主动回访。',
  followup_limit: '待处理事项已达上限，请先整理已有提醒。',
  followup_memory_changed: '相关记忆已经改变，请新建事项。',
  memory_unavailable: '记忆存储暂时不可用，请稍后再试。',
  memory_disabled: '服务端尚未启用长期记忆。',
  invalid_memory: '请填写有效主题和正文，正文最多 2,000 字。',
  memory_key_exists: '这个主题已经存在，请修正原有记忆。',
  memory_not_found: '记忆不存在或已经被遗忘。',
  memory_limit: '记忆数量已达上限，请先整理已有内容。',
  invalid_token: 'Admin token 不正确，请重新输入。',
  invalid_or_expired_link: '这个访问链接已失效或被撤销，请联系管理员。',
  session_expired: '登录已过期，请重新登录。',
  access_expired: '临时访问权限已过期或被撤销。',
  authentication_required: '请先登录。',
  invalid_origin: '当前网页地址与服务端 PUBLIC_URL 不一致。',
  rate_limited: '请求较频繁，请稍等一分钟再试。',
  pending_context_full: '这一轮补充的内容较多，请等回复完成，或停止后重新整理发送。',
  conversation_busy: '这个对话还有多条请求等待处理，请稍后再发送。',
  invalid_message_length: '消息需要包含 1–12,000 个字符。',
  storage_unavailable: '存储暂时不可用，请稍后再试。',
  invalid_link_options: '请填写名称，并设置 1 分钟至 30 天的有效期。',
  request_timeout: '服务响应超时，请重试。',
  service_unavailable: '服务暂时不可用，请稍后重试。',
  conversation_not_found: '这个对话不存在或你没有访问权限。',
};

/** 前端只使用 API 主机的 HttpOnly Cookie，根密钥不写入浏览器持久化存储。 */
export class ApiError extends Error {
  constructor(
    public status: number,
    public code: string,
  ) {
    super(code);
  }
}

/** 统一解析业务错误，网络中断与服务端拒绝都必须反馈给用户。 */
export async function api<T>(path: string, options: RequestInit = {}): Promise<T> {
  const response = await fetch(apiUrl(path), {
    ...options,
    credentials: 'include',
    headers: { 'Content-Type': 'application/json', ...options.headers },
  });
  const data = await response.json().catch(() => ({}));
  if (!response.ok)
    throw new ApiError(
      response.status,
      data.error ||
        (response.status === 504 || response.status === 408
          ? 'request_timeout'
          : response.status >= 500
            ? 'service_unavailable'
            : 'request_failed'),
    );
  return data as T;
}

/** 服务端原始错误不直接用作 HTML，未知故障保持可恢复提示。 */
export function errorText(error: unknown): string {
  if (error instanceof ApiError)
    return ERRORS[error.code] || `操作未完成（${error.code}），请稍后再试。`;
  return '连接暂时中断，请检查网络后重试。';
}
