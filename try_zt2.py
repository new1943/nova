import akshare as ak

# Try stock_zt_pool_previous_em
try:
    df = ak.stock_zt_pool_previous_em(date="20250903")
    print("=== stock_zt_pool_previous_em ===")
    print(f"Columns: {df.columns.tolist()}")
    print(f"Shape: {df.shape}")
    print(df.head(10).to_string())
except Exception as e:
    print(f"stock_zt_pool_previous_em error: {e}")

print()

# Try with no args for today
try:
    df = ak.stock_zt_pool_previous_em()
    print("=== stock_zt_pool_previous_em (today) ===")
    print(f"Columns: {df.columns.tolist()}")
    print(f"Shape: {df.shape}")
    print(df.head(5).to_string())
except Exception as e:
    print(f"stock_zt_pool_previous_em (today) error: {e}")