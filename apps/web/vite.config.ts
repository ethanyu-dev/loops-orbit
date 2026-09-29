import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// 开发时仍由 Rust 处理 API 与 Cookie，浏览器只访问一个源。
export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    strictPort: true,
    proxy: { '/api': 'http://127.0.0.1:8080', '/health': 'http://127.0.0.1:8080' },
  },
});
