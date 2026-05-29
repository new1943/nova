import akshare as ak
import traceback

# Try stock_zdhtmx_em (涨跌停每日)
try:
    df = ak.stock_zdhtmx_em(date="20250903")
    print("=== stock_zdhtmx_em ===")
    print(df.columns.tolist())
    print(df.head(5).to_string())
    print(f"Total rows: {len(df)}")
except Exception as e:
    print(f"stock_zdhtmx_em error: {e}")
    traceback.print_exc()

print()
print("="*50)
print()

# Try stock_zt_pool_em (涨停池)
try:
    df = ak.stock_zt_pool_em(date="20250903")
    print("=== stock_zt_pool_em ===")
    print(df.columns.tolist())
    print(df.head(3).to_string())
except Exception as e:
    print(f"stock_zt_pool_em error: {e}")
    traceback.print_exc()

print()
print("="*50)
print()

# Try stock_zt_pool_strong_em
try:
    df = ak.stock_zt_pool_strong_em(date="20250903")
    print("=== stock_zt_pool_strong_em ===")
    print(df.columns.tolist())
    print(df.head(3).to_string())
except Exception as e:
    print(f"stock_zt_pool_strong_em error: {e}")
    traceback.print_exc()

print()
print("="*50)
print()

# Try stock_zt_pool_dtgc_em (跌停股池)
try:
    df = ak.stock_zt_pool_dtgc_em(date="20250903")
    print("=== stock_zt_pool_dtgc_em ===")
    print(df.columns.tolist())
    print(df.head(3).to_string())
except Exception as e:
    print(f"stock_zt_pool_dtgc_em error: {e}")
    traceback.print_exc()