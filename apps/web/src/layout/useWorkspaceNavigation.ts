import { useCallback } from 'react';
import { useLocation, useMatch, useNavigate } from 'react-router-dom';
import { CONVERSATION_ID, PAGE_PATHS, conversationPath, type Page } from './navigation';

/** URL 是唯一导航状态；React Router 负责刷新恢复和浏览器前进后退。 */
export function useWorkspaceNavigation() {
  const { pathname } = useLocation();
  const navigate = useNavigate();
  const match = useMatch('/chat/:conversationId');
  const id = match?.params.conversationId;
  const selected = id && CONVERSATION_ID.test(id) ? id : null;
  const page =
    (Object.entries(PAGE_PATHS).find(
      ([, path]) => pathname === path || pathname.startsWith(`${path}/`),
    )?.[0] as Page | undefined) ?? 'chat';
  // 首条消息产生的 ID 替换空白会话地址，返回键不会再回到已提交的空白表单。
  const setSelected = useCallback(
    (next: string | null) => {
      void navigate(conversationPath(next), { replace: true });
    },
    [navigate],
  );
  const newChat = useCallback(() => {
    void navigate(PAGE_PATHS.chat);
  }, [navigate]);
  return { page, selected, setSelected, newChat, invalidConversation: !!id && !selected };
}
