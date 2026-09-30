FROM node:24-bookworm-slim AS build
WORKDIR /build
COPY package.json package-lock.json ./
COPY apps/web/package.json apps/web/package.json
RUN npm ci
COPY apps/web apps/web
RUN npm run build --workspace @orbit/web

# 静态资源服务独立于 Rust；非 root 进程监听平台注入的端口。
FROM nginxinc/nginx-unprivileged:stable-alpine AS runtime
COPY --from=build /build/apps/web/dist /usr/share/nginx/html
COPY apps/web/deploy/default.conf.template /etc/nginx/templates/default.conf.template
COPY --chmod=755 apps/web/deploy/19-validate-env.sh /docker-entrypoint.d/19-validate-env.sh
ENV PORT=8080 API_ORIGIN=https://api-orbit.ethankit.com
EXPOSE 8080
