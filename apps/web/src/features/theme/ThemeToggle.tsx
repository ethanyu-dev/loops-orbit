import { useEffect, useState } from 'react';
import { Moon, Sun } from 'lucide-react';
import { readTheme, saveTheme, subscribeTheme } from './theme';

/** 登录页和工作空间共用外观入口；按钮名称描述点击后的效果。 */
export function ThemeToggle() {
  const [theme, setTheme] = useState(readTheme);
  useEffect(() => subscribeTheme(setTheme), []);
  const label = theme === 'dark' ? '切换到浅色模式' : '切换到深色模式';
  return (
    <button
      className="theme-toggle"
      aria-label={label}
      title={label}
      onClick={() => {
        const next = theme === 'dark' ? 'light' : 'dark';
        saveTheme(next);
        setTheme(next);
      }}
    >
      {theme === 'dark' ? <Sun size={17} /> : <Moon size={17} />}
    </button>
  );
}
