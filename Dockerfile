# 第一阶段：构建
FROM ghcr.io/vansour/rust:trixie as builder

WORKDIR /app

# 复制 Cargo.toml 和 Cargo.lock
COPY Cargo.toml Cargo.lock ./

# 创建 src 目录结构
RUN mkdir -p src && echo "fn main() {}" > src/main.rs

# 预先缓存依赖
RUN cargo build --release 2>&1 | grep -E "Compiling|Finished" || true

# 删除虚拟源码
RUN rm -rf src

# 复制真实源代码
COPY src ./src

# 构建实际应用
RUN cargo build --release

# 第二阶段：运行时
FROM ghcr.io/vansour/debian:trixie-slim

# 安装运行时依赖
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# 从构建阶段复制二进制文件
COPY --from=builder /app/target/release/docker-proxy /usr/local/bin/docker-proxy

# 复制 web 静态文件到镜像（用于 web UI）
COPY web ./web

# 暴露端口
EXPOSE 8081

# 设置环境变量
ENV RUST_LOG=info

# 运行应用
CMD ["docker-proxy"]
