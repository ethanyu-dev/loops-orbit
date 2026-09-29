/** 统一显示授权到期时间，使用浏览器所在时区。 */
export const formatDate = (value: string) =>
  new Date(value).toLocaleString('zh-CN', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });
