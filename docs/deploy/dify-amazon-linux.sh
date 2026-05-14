#!/bin/bash
# Dify 纯源码生产部署脚本 - Amazon Linux 专用版
# 目标系统：Amazon Linux 2023 (AL2023)
# 架构：源码跑 Backend/Web + Docker 跑中间件
# 
# 使用方法（复制粘贴整段到终端执行）：
#   chmod +x ~/dify-deploy.sh && ~/dify-deploy.sh
#
# 作者：Cleo 🤍
# 日期：2026-05-12

set -e

# ==================== 配置区 ====================
DEPLOY_USER="${USER:-ec2-user}"
DEPLOY_HOME=$(getent passwd "$DEPLOY_USER" | cut -d: -f6)
DEPLOY_DIR="${DEPLOY_HOME}/dify-deploy"

GIT_REPO="https://github.com/langgenius/dify.git"
GIT_BRANCH="main"

# 中间件（Docker）
POSTGRES_PORT=5432
REDIS_PORT=6379
WEAVIATE_PORT=8080
DB_NAME="dify"
DB_USER="dify"
DB_PASS="Dify2026!@#"

# 服务端口
API_PORT=5000
WEB_PORT=3000

# ==================== 颜色 ====================
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

log()     { echo -e "${GREEN}[  OK  ]${NC} $1"; }
warn()    { echo -e "${YELLOW}[ WARN ]${NC} $1"; }
error()   { echo -e "${RED}[ERROR ]${NC} $1"; }
info()    { echo -e "${BLUE}[ INFO ]${NC} $1"; }
section() { echo ""; echo -e "${CYAN}${BOLD}━━━━━━━━━━━━━━━━ $1${NC}"; }

# ==================== 前置检查 ====================
check_root() {
    if [ "$EUID" -ne 0 ]; then
        error "请用 root 运行此脚本，或加 sudo"
        exit 1
    fi
}

check_os() {
    if ! grep -q "Amazon Linux" /etc/os-release 2>/dev/null; then
        warn "检测到非 Amazon Linux 系统，脚本可能需要调整"
    fi
    log "操作系统：$(cat /etc/os-release | grep PRETTY_NAME | cut -d'"' -f2)"
}

# ==================== 步骤 1：安装基础依赖 ====================
step1_yum_packages() {
    section "步骤 1/9：安装基础依赖"

    # Amazon Linux 2023 基础工具
    yum install -y \
        git \
        curl \
        ca-certificates \
        gcc \
        gcc-c++ \
        make \
        openssl \
        sudo \
        tar \
        gzip \
        PolicycorePython3 \
        checkpolicy 2>/dev/null || true

    log "基础工具安装完成"
}

# ==================== 步骤 2：安装 Docker ====================
step2_docker() {
    section "步骤 2/9：安装 Docker"

    if command -v docker &>/dev/null; then
        log "Docker 已安装：$(docker --version)"
    else
        info "安装 Docker..."
        yum install -y docker
        systemctl enable --now docker
        log "Docker 已安装并启动"
    fi

    # 给部署用户加 Docker 权限
    if ! id -nG "$DEPLOY_USER" | grep -qw docker; then
        usermod -aG docker "$DEPLOY_USER"
        warn "已将 $DEPLOY_USER 加入 docker 组，请重新登录生效"
    fi

    log "Docker 状态：$(systemctl is-active docker)"
}

# ==================== 步骤 3：安装 uv（Python 包管理器）================
step3_uv() {
    section "步骤 3/9：安装 uv（后端 Python 包管理器）"

    if command -v uv &>/dev/null; then
        log "uv 已安装：$(uv --version)"
    else
        info "安装 uv..."
        curl -LsSf https://astral.sh/uv/install.sh | sh

        # 添加到 PATH（写入 bashrc）
        export PATH="$HOME/.local/bin:$PATH"
        if ! grep -q '.local/bin' "$DEPLOY_HOME/.bashrc" 2>/dev/null; then
            echo 'export PATH="$HOME/.local/bin:$PATH"' >> "$DEPLOY_HOME/.bashrc"
        fi
        log "uv 安装完成"
    fi
}

# ==================== 步骤 4：安装 Node.js + pnpm ====================
step4_nodejs() {
    section "步骤 4/9：安装 Node.js + pnpm"

    # 安装 Node.js 22 LTS（pnpm 最新版需要 Node 22+）
    if ! node --version 2>/dev/null | grep -qE "^v2[2-9]"; then
        info "安装 Node.js 22 LTS..."
        curl -fsSL https://rpm.nodesource.com/setup_22.x | bash -
        yum install -y nodejs
    fi
    log "Node.js：$(node -v)"
    log "npm：$(npm -v)"

    # 安装 pnpm（指定兼容 Node 22 的版本）
    info "安装 pnpm..."
    npm install -g pnpm@10 2>/dev/null || npm install -g pnpm@9
    log "pnpm：$(pnpm -v)"
}

# ==================== 步骤 5：克隆 Dify 源码 ====================
step5_clone() {
    section "步骤 5/9：克隆 Dify 源码"

    mkdir -p "$DEPLOY_DIR"
    cd "$DEPLOY_DIR"

    if [ -d "$DEPLOY_DIR/dify/.git" ]; then
        warn "Dify 已存在，跳过克隆"
        cd "$DEPLOY_DIR/dify"
        git pull origin "$GIT_BRANCH"
    else
        info "克隆 Dify（分支：$GIT_BRANCH）..."
        git clone --depth 1 --branch "$GIT_BRANCH" "$GIT_REPO" "$DEPLOY_DIR/dify"
        cd "$DEPLOY_DIR/dify"
    fi

    log "当前版本：$(git log --oneline -1)"
}

# ==================== 步骤 6：启动中间件（PostgreSQL + Redis + Weaviate）================
step6_middleware() {
    section "步骤 6/9：启动中间件（Docker）"

    # 生成密钥
    SECRET_KEY=$(openssl rand -base64 42)

    # 写 .env
    cat > "$DEPLOY_DIR/dify/docker/.env" << EOF
# ============ 中间件 ============
DB_USERNAME=${DB_USER}
DB_PASSWORD=${DB_PASS}
DB_HOST=localhost
DB_PORT=${POSTGRES_PORT}
DB_DATABASE=${DB_NAME}
# PostgreSQL Docker 容器密码
POSTGRES_PASSWORD=${DB_PASS}

REDIS_HOST=localhost
REDIS_PASSWORD=DifyRedis2026!@#

WEAVIATE_URL=http://localhost:${WEAVIATE_PORT}

# ============ 安全配置 ============
SECRET_KEY=${SECRET_KEY}
CONSOLE_WEB_URL=http://localhost:${WEB_PORT}
CONSOLE_API_URL=http://localhost:${API_PORT}/console/api
APP_API_URL=http://localhost:${API_PORT}/v1
APP_WEB_URL=http://localhost:${WEB_PORT}

# ============ 初始化（首次设为 true）============
INIT_PASSWORD=DifyAdmin2026!@#
INITIALIZE=true
EOF

    # 拉取镜像并启动
    info "启动 PostgreSQL 16..."
    docker run -d \
        --name dify-postgres \
        -p ${POSTGRES_PORT}:5432 \
        -e POSTGRES_USER="${DB_USER}" \
        -e POSTGRES_PASSWORD="${DB_PASS}" \
        -e POSTGRES_DB="${DB_NAME}" \
        --restart unless-stopped \
        postgres:16-alpine

    info "启动 Redis 7..."
    docker run -d \
        --name dify-redis \
        -p ${REDIS_PORT}:6379 \
        -e REDIS_PASSWORD="DifyRedis2026!@#" \
        --restart unless-stopped \
        redis:7-alpine redis-server --requirepass "DifyRedis2026!@#"

    info "启动 Weaviate（向量数据库）..."
    docker run -d \
        --name dify-weaviate \
        -p ${WEAVIATE_PORT}:8080 \
        -e ENABLE_MODULES=text2vec-transformers \
        -e TRANSFORMERS_INFERENCE_API=http://t2v-transformers:8080 \
        --restart unless-stopped \
        semitechnologies/weaviate:latest

    # 等待启动
    info "等待中间件就绪（约20秒）..."
    sleep 20

    # 验证
    docker exec dify-postgres pg_isready -U "${DB_USER}" &>/dev/null && \
        log "✓ PostgreSQL 就绪" || error "PostgreSQL 启动失败"

    docker exec dify-redis redis-cli -a "DifyRedis2026!@#" ping 2>/dev/null | grep -q PONG && \
        log "✓ Redis 就绪" || error "Redis 启动失败"

    curl -sf http://localhost:${WEAVIATE_PORT}/v1/.well-known/ready > /dev/null && \
        log "✓ Weaviate 就绪" || warn "Weaviate 未就绪（可后续处理）"
}

# ==================== 步骤 7：安装后端依赖 ====================
step7_backend_deps() {
    section "步骤 7/9：安装后端依赖（uv sync）"

    cd "$DEPLOY_DIR/dify/api"

    # Python 3.11+ 推荐（Amazon Linux 默认 Python 3.9，需安装 3.11+）
    if ! python3 --version 2>/dev/null | grep -qE "3\.(11|12|13)"; then
        warn "检测到 Python $(python3 --version 2>&1 | grep -oE '[0-9]+\.[0-9]+')，推荐安装 Python 3.11+"

        # Amazon Linux 2023 用 amazon-linux-extras 安装 Python 3.11
        if command -v amazon-linux-extras &>/dev/null; then
            # 先禁用有问题的 MySQL 源，避免 404 干扰
            yum config-manager --disable mysql* 2>/dev/null || true
            # 安装 Python 3.11（pip 包含在 python311 包里，不需要单独装）
            amazon-linux-extras install python3.11 -y 2>/dev/null || \
                yum install -y python3.11 --enablerepo=codeengine 2>/dev/null || \
                yum install -y python3.11
        fi
    fi

    export PATH="$HOME/.local/bin:$PATH"

    # 确保 uv 使用 Python 3.11
    if command -v python3.11 &>/dev/null; then
        export UV_PYTHON=python3.11
        info "uv 将使用 Python $(python3.11 --version)"
    fi

    info "安装 Python 依赖（uv sync）..."
    uv sync --all-groups

    log "后端依赖安装完成！"
}

# ==================== 步骤 8：构建前端 ====================
step8_frontend() {
    section "步骤 8/9：安装前端依赖并构建"

    cd "$DEPLOY_DIR/dify"

    info "安装前端依赖（pnpm install）..."
    pnpm install

    # 前端环境变量
    cat > web/.env.local << EOF
NEXT_PUBLIC_API_PREFIX=http://localhost:${API_PORT}
NEXT_PUBLIC_PUBLIC_API_PREFIX=http://localhost:${API_PORT}
NEXT_PUBLIC_COOKIE_DOMAIN=
NEXT_PUBLIC_APP_BUILD_MODE=EXTERNAL
NEXT_PUBLIC_ENABLE_MRQ_EXECUTION=false
EOF

    info "构建前端（pnpm run build）..."
    pnpm -C web run build

    log "前端构建完成！"
}

# ==================== 步骤 9：生成服务脚本 ====================
step9_service() {
    section "步骤 9/9：生成 systemd 服务"

    export PATH="$HOME/.local/bin:$PATH"

    # ===== 创建目录 =====
    mkdir -p "$DEPLOY_DIR/logs/api"
    mkdir -p "$DEPLOY_DIR/logs/worker"
    mkdir -p "$DEPLOY_DIR/pids"

    # ===== 创建启动脚本 =====
    cat > "$DEPLOY_DIR/start.sh" << 'STARTSCRIPT'
#!/bin/bash
# Dify 启动脚本
# 位置：~/dify-deploy/start.sh

export PATH="$HOME/.local/bin:$PATH"
cd ~/dify-deploy/dify/api

echo "[Dify] 启动后端 API (Gunicorn)..."
uv run gunicorn app:app \
    --bind 0.0.0.0:5000 \
    --workers 2 \
    --worker-class uvicorn.workers.UvicornWorker \
    --access-logfile ~/dify-deploy/logs/api/access.log \
    --error-logfile ~/dify-deploy/logs/api/error.log \
    --daemon \
    --pid ~/dify-deploy/pids/api.pid

echo "[Dify] 启动 Worker..."
uv run celery -A app.celery worker \
    --loglevel=info \
    --detach \
    --logfile ~/dify-deploy/logs/worker/worker.log \
    --pidfile ~/dify-deploy/pids/worker.pid

echo "[Dify] 启动 Beat（定时任务）..."
uv run celery -A app.celery beat \
    --loglevel=info \
    --detach \
    --logfile ~/dify-deploy/logs/worker/beat.log \
    --pidfile ~/dify-deploy/pids/beat.pid

# 等待后端启动
sleep 3

# 前端
echo "[Dify] 启动前端..."
cd ~/dify-deploy/dify
pnpm -C web run start --port 3000 --hostname 0.0.0.0 &

echo ""
echo "✅ Dify 启动完成！"
echo "   后端 API：http://localhost:5000"
echo "   前端 Web：  http://localhost:3000"
echo "   API 文档： http://localhost:5000/docs"
echo ""
STARTSCRIPT

    # ===== 停止脚本 =====
    cat > "$DEPLOY_DIR/stop.sh" << 'STOPSCRIPT'
#!/bin/bash
cd ~/dify-deploy

echo "[Dify] 停止所有进程..."
[ -f pids/api.pid ]   && kill $(cat pids/api.pid) 2>/dev/null
[ -f pids/worker.pid ] && kill $(cat pids/worker.pid) 2>/dev/null
[ -f pids/beat.pid ]   && kill $(cat pids/beat.pid) 2>/dev/null
pkill -f "gunicorn.*app:app" 2>/dev/null || true
pkill -f "celery.*worker" 2>/dev/null || true
pkill -f "celery.*beat" 2>/dev/null || true
pkill -f "next start" 2>/dev/null || true

echo "[Dify] 停止中间件..."
docker stop dify-postgres dify-redis dify-weaviate 2>/dev/null

echo "✅ 已停止"
STOPSCRIPT

    # ===== systemd 服务文件 =====
    cat > /etc/systemd/system/dify-api.service << 'APISERVICE'
[Unit]
Description=Dify Backend API
After=network.target docker.service
Requires=docker.service

[Service]
Type=simple
User=ec2-user
Environment="PATH=/home/ec2-user/.local/bin:/usr/local/bin:/usr/bin:/bin"
WorkingDirectory=/home/ec2-user/dify-deploy/dify/api
ExecStart=/home/ec2-user/.local/bin/uv run gunicorn app:app \
    --bind 0.0.0.0:5000 \
    --workers 2 \
    --worker-class uvicorn.workers.UvicornWorker \
    --access-logfile /home/ec2-user/dify-deploy/logs/api/access.log \
    --error-logfile /home/ec2-user/dify-deploy/logs/api/error.log \
    --pid /home/ec2-user/dify-deploy/pids/api.pid
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
APISERVICE

    cat > /etc/systemd/system/dify-worker.service << 'WORKERSERVICE'
[Unit]
Description=Dify Celery Worker
After=network.target docker.service
Requires=dify-api.service

[Service]
Type=simple
User=ec2-user
Environment="PATH=/home/ec2-user/.local/bin:/usr/local/bin:/usr/bin:/bin"
WorkingDirectory=/home/ec2-user/dify-deploy/dify/api
ExecStart=/home/ec2-user/.local/bin/uv run celery -A app.celery worker \
    --loglevel=info \
    --logfile /home/ec2-user/dify-deploy/logs/worker/worker.log \
    --pidfile /home/ec2-user/dify-deploy/pids/worker.pid
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
WORKERSERVICE

    cat > /etc/systemd/system/dify-web.service << 'WEBSERVICE'
[Unit]
Description=Dify Frontend Web
After=network.target
Requires=dify-api.service

[Service]
Type=simple
User=ec2-user
WorkingDirectory=/home/ec2-user/dify-deploy/dify
ExecStart=/usr/bin/bash -c 'cd /home/ec2-user/dify-deploy/dify && /usr/local/bin/pnpm -C web run start --port 3000 --hostname 0.0.0.0'
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
WEBSERVICE

    chmod +x "$DEPLOY_DIR/start.sh"
    chmod +x "$DEPLOY_DIR/stop.sh"

    systemctl daemon-reload
    systemctl enable dify-api dify-worker dify-web 2>/dev/null || true

    log "systemd 服务已配置并设为开机启动"
}

# ==================== 主流程 ====================
main() {
    clear
    echo ""
    echo -e "${CYAN}${BOLD}"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "    🤍  Dify 纯源码部署 — Amazon Linux"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo -e "${NC}"
    echo "  部署目录：$DEPLOY_DIR"
    echo "  部署用户：$DEPLOY_USER"
    echo ""

    check_root
    check_os
    step1_yum_packages
    step2_docker
    step3_uv
    step4_nodejs
    step5_clone
    step6_middleware
    step7_backend_deps
    step8_frontend
    step9_service

    echo ""
    echo -e "${GREEN}${BOLD}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "  ✅ 部署完成！"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo ""
    echo -e "${BOLD}📋 部署摘要：${NC}"
    echo "   项目目录：$DEPLOY_DIR/dify"
    echo "   PostgreSQL：localhost:${POSTGRES_PORT}（Docker）"
    echo "   Redis：     localhost:${REDIS_PORT}（Docker）"
    echo "   Weaviate：  localhost:${WEAVIATE_PORT}（Docker）"
    echo "   后端 API：  http://localhost:${API_PORT}"
    echo "   前端 Web：  http://localhost:${WEB_PORT}"
    echo ""
    echo -e "${BOLD}🚀 启动方式：${NC}"
    echo ""
    echo "  方式 A — systemd（推荐生产环境）："
    echo "    sudo systemctl start dify-api dify-worker dify-web"
    echo "    sudo systemctl status dify-api"
    echo ""
    echo "  方式 B — 手动脚本："
    echo "    cd $DEPLOY_DIR && ./start.sh"
    echo ""
    echo -e "${BOLD}📝 首次初始化（必须执行一次）：${NC}"
    echo "    cd $DEPLOY_DIR/dify/api"
    echo "    source ~/.local/bin/activate 2>/dev/null || true"
    echo "    uv run flask db upgrade"
    echo "    uv run flask admin create --username admin --password <你的密码> --email admin@example.com"
    echo ""
    echo -e "${BOLD}🌐 访问地址：${NC}"
    echo "    http://<你的EC2公网IP>:3000/install"
    echo ""
    echo -e "${BOLD}⚠️  重要提醒：${NC}"
    echo "    1. 安全组需开放 3000/5000 端口"
    echo "    2. 建议用 Nginx + HTTPS 代理前端"
    echo "    3. PostgreSQL/Redis 对外不要暴露端口"
    echo ""
}

main "$@"
