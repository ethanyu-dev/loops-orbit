/** 应用内功能页标识，由应用和布局组件共享。 */
export type Page =
  | 'chat'
  | 'links'
  | 'status'
  | 'memory'
  | 'knowledge'
  | 'followups'
  | 'communications'
  | 'integrations';

// 路由只承载导航；会话归属与管理员权限始终由 API 检查。
export const CONVERSATION_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
export const PAGE_PATHS: Record<Page, string> = {
  chat: '/chat',
  memory: '/memory',
  knowledge: '/knowledge',
  followups: '/todos',
  communications: '/communications',
  integrations: '/integrations',
  links: '/links',
  status: '/status',
};

/** 会话详情地址可复制或刷新，不包含身份凭证。 */
export function conversationPath(id: string | null): string {
  return id && CONVERSATION_ID.test(id) ? `/chat/${id}` : PAGE_PATHS.chat;
}

/** 兼容旧版会话片段和资料引用；未知路径保留给路由的不存在页面处理。 */
export function migrateLegacyLocation(url: URL): string {
  const chat = new URLSearchParams(url.hash.slice(1)).get('chat');
  if (url.pathname === '/') {
    url.pathname =
      url.searchParams.has('feishu') || url.searchParams.has('communication')
        ? PAGE_PATHS.communications
        : conversationPath(chat);
    if (chat) url.hash = '';
  }
  return `${url.pathname}${url.search}${url.hash}`;
}
