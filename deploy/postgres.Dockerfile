# 保留开发数据库原有 Alpine/PG17 基础，避免复用卷时改变 libc 排序规则。
FROM postgres:17-alpine
ARG PGVECTOR_VERSION=0.8.6
RUN apk add --no-cache --virtual .vector-build build-base git \
    && git clone --branch v${PGVECTOR_VERSION} --depth 1 https://github.com/pgvector/pgvector.git /tmp/pgvector \
    && cd /tmp/pgvector \
    && make OPTFLAGS="" with_llvm=no \
    && make install with_llvm=no \
    && cd / \
    && rm -rf /tmp/pgvector \
    && apk del .vector-build
