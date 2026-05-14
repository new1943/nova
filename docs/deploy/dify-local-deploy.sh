#!/bin/bash
# Dify 纯源码生产部署脚本
# 依赖：Docker（中间件）+ 源码（Backend/Web）
# 作者：Cleo 🤍
# 日期：2026-05-12

set -e

# ==================== 配置区 ====================
PROJECT_DIR="$HOME/Dify/deploy"           # 部署目录
GIT_REPO="https://github.com/langgenius/dify.git"
BRANCH="main"                             # 或指定版本 tag，如 v1.0.0
API_PORT=5000
WEB_PORT=3000
MIDDLEWARE_NETWORK="dify-network"

# 中间件配置
POSTGRES_DB="dify"
POSTGRES_USER="dify"
POSTGRES_PASSWORD="dify_production_pass"
POSTGRES_PORT=5432
REDIS_PORT=6379

# Dify 域名（生产环境改成你的域名）
NEXT_PUBLIC_API_PREFIX="http://localhost:${API_PORT}"
NEXT_PUBLIC_PUBLIC_API_PREFIX="http://localhost:${API_PORT}"
COOKIE_DOMAIN="localhost"

# ==================== 颜色 ====================
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log() { echo -e "${GREEN}[部署]${NC} $1"; }
warn() { echo -e "${YELLOW}[警告]${NC} $1"; }
error() { echo -e "${RED}[错误]${NC} $1"; }
info() { echo -e "${BLUE}[信息]${NC} $1"; }

# ==================== 步骤 1：准备目录 ====================
step1_prepare() {
    log "步骤 1：准备目录..."
    mkdir -p "$PROJECT_DIR"
    cd "$PROJECT_DIR"
    log "部署目录：$PROJECT_DIR"
}

# ==================== 步骤 2：克隆代码 ====================
step2_clone() {
    log "步骤 2：克隆 Dify 源码..."
    if [ -d "$PROJECT_DIR/dify/.git" ]; then
        warn "Dify 已存在，跳过克隆。手动更新请运行：cd $PROJECT_DIR/dify && git pull"
    else
        git clone --depth 1 --branch "$BRANCH" "$GIT_REPO" "$PROJECT_DIR/dify"
    fi
    cd "$PROJECT_DIR/dify"
    log "Dify 版本：$(git log --oneline -1)"
}

# ==================== 步骤 3：启动中间件（Docker） ====================
step3_middleware() {
    log "步骤 3：启动中间件（PostgreSQL + Redis + Weaviate）..."

    # 创建 Docker 网络
    docker network create "$MIDDLEWARE_NETWORK" 2>/dev/null || true

    # 生成随机密钥
    SECRET_KEY=$(openssl rand -base64 42)

    # 写 .env 文件
    cat > "$PROJECT_DIR/dify/docker/.env" << EOF
# ============ 中间件配置 ============
# PostgreSQL
DB_USERNAME=${POSTGRES_USER}
DB_PASSWORD=${POSTGRES_PASSWORD}
DB_HOST=postgres
DB_PORT=${POSTGRES_PORT}
DB_DATABASE=${POSTGRES_DB}

# Redis
REDIS_HOST=redis
REDIS_PORT=${REDIS_PORT}
REDIS_PASSWORD=dify_redis_pass

# Weaviate（向量数据库）
WEAVIATE_URL=http://weaviate:8080

# ============ 安全配置 ============
SECRET_KEY=${SECRET_KEY}
CONSOLE_WEB_URL=http://localhost:${WEB_PORT}
CONSOLE_API_URL=http://localhost:${API_PORT}/console/api
APP_API_URL=http://localhost:${API_PORT}/v1
APP_WEB_URL=http://localhost:${WEB_PORT}

# ============ 初始化（仅首次运行设为 true）============
INIT_PASSWORD=admin123
INITIALIZE=true
EOF

    # 启动中间件容器
    log "启动 PostgreSQL..."
    docker run -d \
        --name dify-postgres \
        --network "$MIDDLEWARE_NETWORK" \
        -e POSTGRES_USER="${POSTGRES_USER}" \
        -e POSTGRES_PASSWORD="${POSTGRES_PASSWORD}" \
        -e POSTGRES_DB="${POSTGRES_DB}" \
        -p "${POSTGRES_PORT}:5432" \
        --restart unless-stopped \
        postgres:16-alpine

    log "启动 Redis..."
    docker run -d \
        --name dify-redis \
        --network "$MIDDLEWARE_NETWORK" \
        -e REDIS_PASSWORD=dify_redis_pass \
        -p "${REDIS_PORT}:6379" \
        --restart unless-stopped \
        redis:7-alpine redis-server --requirepass dify_redis_pass

    log "启动 Weaviate（向量数据库）..."
    docker run -d \
        --name dify-weaviate \
        --network "$MIDDLEWARE_NETWORK" \
        -p 8081:8080 \
        -e ENABLE_MODULES=text2vec-transformers \
        -e TRANSFORMERS_INFERENCE_API=http://t2v-transformers:8080 \
        --restart unless-stopped \
        semitechnologies/weaviate:latest

    log "等待中间件启动（15秒）..."
    sleep 15

    # 验证中间件
    docker exec dify-postgres pg_isready -U "${POSTGRES_USER}" && log "✓ PostgreSQL 就绪" || error "PostgreSQL 启动失败"
    docker exec dify-redis redis-cli -a dify_redis_pass ping 2>/dev/null | grep -q PONG && log "✓ Redis 就绪" || error "Redis 启动失败"
    curl -s http://localhost:8081/v1/.well-known/ready 2>/dev/null | grep -q true && log "✓ Weaviate 就绪" || warn "Weaviate 可能未就绪（不影响基本功能）"
}

# ==================== 步骤 4：安装后端依赖 ====================
step4_backend_deps() {
    log "步骤 4：安装后端依赖..."
    cd "$PROJECT_DIR/dify/api"

    # 确保 uv 可用
    if ! command -v uv &>/dev/null; then
        error "uv 未安装，请先运行：curl -LsSf https://astral.sh/uv/install.sh | sh"
        exit 1
    fi

    # 安装 Python 依赖（包含所有 group）
    uv sync --all-groups

    log "后端依赖安装完成！"
}

# ==================== 步骤 5：安装前端依赖 ====================
step5_frontend_deps() {
    log "步骤 5：安装前端依赖..."
    cd "$PROJECT_DIR/dify"

    # 确保 pnpm 可用
    if ! command -v pnpm &>/dev/null; then
        warn "pnpm 未安装，正在安装..."
        npm install -g pnpm
    fi

    # 安装依赖
    pnpm install

    log "前端依赖安装完成！"
}

# ==================== 步骤 6：配置前端环境 ====================
step6_frontend_config() {
    log "步骤 6：配置前端环境变量..."
    cd "$PROJECT_DIR/dify"

    cat > web/.env.local << EOF
# API 配置
NEXT_PUBLIC_API_PREFIX=${NEXT_PUBLIC_API_PREFIX}
NEXT_PUBLIC_PUBLIC_API_PREFIX=${NEXT_PUBLIC_PUBLIC_API_PREFIX}

# Cookie 配置
NEXT_PUBLIC_COOKIE_DOMAIN=${COOKIE_DOMAIN}

# 功能开关
NEXT_PUBLIC_APP_BUILD_MODE=EXTERNAL
NEXT_PUBLIC_ENABLE_MRQ_EXECUTION=false
EOF

    log "前端环境变量配置完成！"
}

# ==================== 步骤 7：构建前端 ====================
step7_frontend_build() {
    log "步骤 7：构建前端生产版本..."
    cd "$PROJECT_DIR/dify"
    pnpm -C web run build
    log "前端构建完成！"
}

# ==================== 步骤 8：生成启动脚本 ====================
step8_scripts() {
    log "步骤 8：生成启动/停止脚本..."

    # 启动脚本
    cat > "$PROJECT_DIR/start.sh" << 'STARTSCRIPT'
#!/bin/bash
# Dify 启动脚本
cd "$(dirname "$0")/dify"

echo "[Dify] 启动后端 API..."
cd api
uv run gunicorn app:app \
    --bind 0.0.0.0:5000 \
    --workers 2 \
    --worker-class uvicorn.workers.UvicornWorker \
    --access-logfile - \
    --error-logfile - \
    --daemon \
    --pid api.pid

echo "[Dify] 启动 Worker..."
uv run celery -A app.celery worker \
    --loglevel=info \
    --detach \
    --pidfile worker.pid

echo "[Dify] 启动 Beat（定时任务）..."
uv run celery -A app.celery beat \
    --loglevel=info \
    --detach \
    --pidfile beat.pid

echo "[Dify] 启动前端 Web..."
cd ..
pnpm -C web run start --port 3000 --hostname 0.0.0.0 &

echo ""
echo "✅ Dify 已启动！"
echo "   前端：http://localhost:3000"
echo "   后端：http://localhost:5000"
echo "   API Docs：http://localhost:5000/docs"
echo ""
echo "首次访问 http://localhost:3000/install 初始化管理员账号"
STARTSCRIPT

    # 停止脚本
    cat > "$PROJECT_DIR/stop.sh" << 'STOPSCRIPT'
#!/bin/bash
# Dify 停止脚本
cd "$(dirname "$0")/dify"

echo "[Dify] 停止后端进程..."
[ -f api/api.pid ] && kill $(cat api/api.pid) 2>/dev/null
[ -f api/worker.pid ] && kill $(cat api/worker.pid) 2>/dev/null
[ -f api/beat.pid ] && kill $(cat api/beat.pid) 2>/dev/null
pkill -f "gunicorn.*app:app" 2>/dev/null
pkill -f "celery.*worker" 2>/dev/null
pkill -f "celery.*beat" 2>/dev/null
pkill -f "next start" 2>/dev/null

echo "[Dify] 停止中间件..."
docker stop dify-postgres dify-redis dify-weaviate 2>/dev/null

echo "✅ Dify 已停止"
STOPSCRIPT

    # 查看日志脚本
    cat > "$PROJECT_DIR/logs.sh" << 'LOGSSCRIPT'
#!/bin/bash
# Dify 日志查看脚本
cd "$(dirname "$0")/dify"
echo "=== 后端日志 (Ctrl+C 退出) ==="
tail -f api/logs/*.log 2>/dev/null || echo "无日志文件，请检查 api/logs/ 目录"
LOGSSCRIPT

    chmod +x "$PROJECT_DIR/start.sh" "$PROJECT_DIR/stop.sh" "$PROJECT_DIR/logs.sh"
    log "启动/停止脚本已生成："
    echo "   启动：$PROJECT_DIR/start.sh"
    echo "   停止：$PROJECT_DIR/stop.sh"
    echo "   日志：$PROJECT_DIR/logs.sh"
}

# ==================== 主流程 ====================
main() {
    clear
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "     🤍  Dify 纯源码生产部署脚本"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""

    step1_prepare
    step2_clone
    step3_middleware
    step4_backend_deps
    step5_frontend_deps
    step6_frontend_config
    step7_frontend_build
    step8_scripts

    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "  ✅ 部署完成！"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""
    echo "📋 部署摘要："
    echo "   项目位置：$PROJECT_DIR/dify"
    echo "   PostgreSQL：localhost:${POSTGRES_PORT}"
    echo "   Redis：     localhost:${REDIS_PORT}"
    echo "   Weaviate：  localhost:8081"
    echo "   后端 API：  http://localhost:${API_PORT}"
    echo "   前端 Web：  http://localhost:${WEB_PORT}"
    echo ""
    echo "🚀 启动命令："
    echo "   $PROJECT_DIR/start.sh"
    echo ""
    echo "📝 首次使用："
    echo "   1. 访问 http://localhost:${WEB_PORT}/install"
    echo "   2. 设置管理员账号"
    echo "   3. 开始使用 Dify！"
    echo ""
}

main "$@"
