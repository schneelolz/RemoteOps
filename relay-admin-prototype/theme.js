// 在加载样式前应用主题，避免首次绘制闪烁。
(function() {
  try {
    let savedTheme = 'system';
    try { savedTheme = localStorage.getItem('remoteops-theme') || 'system'; } catch (_) {}
    const theme = savedTheme === 'dark' || (savedTheme !== 'light' && window.matchMedia('(prefers-color-scheme: dark)').matches) ? 'dark' : 'light';
    document.documentElement.setAttribute('data-theme', theme);
    document.documentElement.style.colorScheme = theme;
  } catch (_) {}
})();
