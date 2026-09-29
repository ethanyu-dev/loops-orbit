// 仅持久化外观偏好，不与账号凭证或业务数据共用存储。
const THEME_KEY = 'orbit.theme';
const SYSTEM_THEME = '(prefers-color-scheme: dark)';
export type Theme = 'light' | 'dark';

/** 无有效偏好时跟随系统；浏览器禁用存储也不影响进入应用。 */
export function readTheme(): Theme {
  try {
    const saved = localStorage.getItem(THEME_KEY);
    if (saved === 'light' || saved === 'dark') return saved;
  } catch {
    /* 隐私模式可能拒绝读取本地存储。 */
  }
  return matchMedia(SYSTEM_THEME).matches ? 'dark' : 'light';
}

/** 在首轮渲染前应用主题，原生控件和浏览器主题色保持一致。 */
export function applyTheme(theme: Theme) {
  document.documentElement.dataset.theme = theme;
  document.documentElement.style.colorScheme = theme;
  document
    .querySelector('meta[name="theme-color"]')
    ?.setAttribute('content', theme === 'dark' ? '#0a0a0a' : '#ffffff');
}

/** 本地存储失败时仍保留本次页面的选择。 */
export function saveTheme(theme: Theme) {
  applyTheme(theme);
  try {
    localStorage.setItem(THEME_KEY, theme);
  } catch {
    /* 当前页切换不依赖存储成功。 */
  }
}

/** 同步其他标签页，以及尚未手选主题时的系统外观变化。 */
export function subscribeTheme(update: (theme: Theme) => void) {
  const media = matchMedia(SYSTEM_THEME);
  const sync = () => {
    const theme = readTheme();
    applyTheme(theme);
    update(theme);
  };
  const storage = (event: StorageEvent) => {
    if (event.key === THEME_KEY || event.key === null) sync();
  };
  media.addEventListener('change', sync);
  window.addEventListener('storage', storage);
  return () => {
    media.removeEventListener('change', sync);
    window.removeEventListener('storage', storage);
  };
}
