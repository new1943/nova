#!/bin/bash
# Dify 后端初始化脚本
# 首次部署后必须执行一次，用于数据库迁移和创建管理员账号
#
# 使用方法：
#   cd ~/dify-deploy/dify/api
#   source ~/.bashrc    # 加载 uv PATH
#   bash ~/dify-deploy/init.sh

set -e

export PATH="$HOME/.local/bin:$PATH"
cd ~/dify-deploy/dify/api

ADMIN_PASS="${1:-DifyAdmin2026!@#}"
ADMIN_EMAIL="${2:-admin@example.com}"

echo "🔧 Dify 后端初始化..."
echo ""

# 等待 PostgreSQL
echo "等待 PostgreSQL 就绪..."
for i in {1..30}; do
    if docker exec dify-postgres pg_isready -U dify &>/dev/null; then
        echo "✓ PostgreSQL 就绪"
        break
    fi
    [ $i -eq 30 ] && echo "⚠ PostgreSQL 未就绪，继续尝试..." || sleep 1
done

# 运行数据库迁移
echo ""
echo "运行数据库迁移（flask db upgrade）..."
uv run flask db upgrade

# 创建管理员
echo ""
echo "创建管理员账号..."
echo "  用户名：admin"
echo "  密码：$ADMIN_PASS"
echo "  邮箱：$ADMIN_EMAIL"
echo ""
uv run flask admin create \
    --username admin \
    --password "$ADMIN_PASS" \
    --email "$ADMIN_EMAIL"

echo ""
echo "✅ 初始化完成！"
echo "   访问 http://localhost:3000 或 http://<EC2公网IP>:3000"
echo "   使用 admin / $ADMIN_PASS 登录"
