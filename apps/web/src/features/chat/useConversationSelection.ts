import { useCallback, useEffect, useState } from 'react';

// 会话 ID 只恢复导航，不提供访问权限；实际归属始终由服务端认证校验。
const CONVERSATION_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** 只接受 UUID 导航参数，忽略其他片段，临时授权 token 已在入口移除。 */
function readSelection(): string | null {
  const id = new URLSearchParams(location.hash.slice(1)).get('chat');
  return id && CONVERSATION_ID.test(id) ? id : null;
}

/** 把当前会话放入 URL 片段，刷新后继续查看原任务，不重新提交消息。 */
export function useConversationSelection() {
  const [selected, update] = useState<string | null>(readSelection);
  const setSelected = useCallback((id: string | null) => {
    update(id);
    history.replaceState(
      null,
      '',
      `${location.pathname}${location.search}${id ? `#chat=${id}` : ''}`,
    );
  }, []);
  useEffect(() => {
    const restore = () => update(readSelection());
    window.addEventListener('hashchange', restore);
    return () => window.removeEventListener('hashchange', restore);
  }, []);
  return [selected, setSelected] as const;
}
