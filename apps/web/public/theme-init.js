// 在样式和应用脚本加载前设置外观，避免已选择深色时出现浅色首屏。
// 与 src/features/theme/theme.ts 保持相同的存储键及默认策略；不读取凭证。
(() => {
  let theme;
  try {
    theme = localStorage.getItem('orbit.theme');
  } catch {}
  if (theme !== 'light' && theme !== 'dark') {
    theme = matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
  }
  document.documentElement.dataset.theme = theme;
  document.documentElement.style.colorScheme = theme;
})();
