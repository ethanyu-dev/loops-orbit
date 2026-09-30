import { createRoot } from 'react-dom/client';
import { App } from './App';
import { BrowserRouter } from 'react-router-dom';
import { migrateLegacyLocation } from './layout/navigation';
import './theme.css';
import './styles.css';
import './console.css';
import { applyTheme, readTheme } from './features/theme/theme';

applyTheme(readTheme());

// 临时 token 在首轮渲染前移除，避免刷新、复制地址或引用跳转时意外携带。
const incomingToken = new URLSearchParams(location.hash.slice(1)).get('token');
if (incomingToken) history.replaceState(null, '', location.pathname + location.search);

history.replaceState(null, '', migrateLegacyLocation(new URL(location.href)));

createRoot(document.getElementById('root')!).render(
  <BrowserRouter>
    <App incomingToken={incomingToken} />
  </BrowserRouter>,
);
