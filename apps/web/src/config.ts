/** 静态服务器注入的公开配置，不允许放入密钥。 */
declare global {
  interface Window {
    /** 在入口脚本执行前加载，容器重启即可切换 API 地址。 */
    __ORBIT_CONFIG__?: { apiOrigin: string };
  }
}

/** 只允许完整 HTTP(S) 源，避免路径拼接把凭据发向非预期地址。 */
function readApiOrigin(): string {
  const value = import.meta.env.VITE_API_ORIGIN || window.__ORBIT_CONFIG__?.apiOrigin;
  if (!value) throw new Error('缺少 API 地址配置');
  const url = new URL(value);
  if (url.origin !== value || !['http:', 'https:'].includes(url.protocol))
    throw new Error('API 地址必须是无路径的 HTTP(S) 源');
  return url.origin;
}

// 构建时可覆盖本地配置；生产镜像默认由 Nginx 的 API_ORIGIN 注入。
export const API_ORIGIN = readApiOrigin();

/** API 请求和授权回调说明复用相同地址来源。 */
export function apiUrl(path: string): string {
  return `${API_ORIGIN}/api${path}`;
}
