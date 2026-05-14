#!/bin/bash
# Dify 后端初始化脚本（首次运行必须执行）
# 运行 migrate 和 init-admin

set -e

DEPLOY_DIR="$HOME/Dify/deploy/dify"

echo "🔧 初始化 Dify 后端..."

cd "$DEPLOY_DIR/api"

# 等待 PostgreSQL 就绪
echo "等待 PostgreSQL..."
for i in {1..30}; do
    if docker exec dify-postgres pg_isready -U dify -d dify &>/dev/null; then
        echo "✓ PostgreSQL 就绪"
        break
    fi
    sleep 1
done

# 运行数据库迁移
echo "运行数据库迁移..."
uv run flask db upgrade

# 初始化管理员
echo "初始化管理员账号..."
uv run flask admin create \
    --username admin \
    --password ADMIN_PASSWORD_HERE \
    --email admin@example.com

echo "✅ 初始化完成！"
echo "访问 http://localhost:3000/install 完成配置"
