import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// 开发与生产都直接请求独立 API，提前验证跨源凭据和来源校验。
export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    strictPort: true,
  },
});
