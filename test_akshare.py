import akshare as ak
import json

# Try stock_market_activity_legu
try:
    df = ak.stock_market_activity_legu()
    print("=== stock_market_activity_legu ===")
    print(df.tail(30).to_string())
except Exception as e:
    print(f"legu error: {e}")

print()

# Try stock_market_activity_em
try:
    df = ak.stock_market_activity_em()
    print("=== stock_market_activity_em ===")
    print(df.tail(30).to_string())
except Exception as e:
    print(f"em error: {e}")

print()

# Try stock_board_industry_spot_em to get advance/decline data
try:
    df = ak.stock_zh_a_spot_em()
    print("=== stock_zh_a_spot_em ===")
    print(f"Columns: {list(df.columns)}")
    print(df.head(3).to_string())
except Exception as e:
    print(f"spot error: {e}")

print()

# Try market breadth from east money
try:
    df = ak.stock_em_zh_a_growth_index()
    print("=== stock_em_zh_a_growth_index ===")
    print(df.tail(20).to_string())
except Exception as e:
    print(f"growth_index error: {e}")

print()

# Try historical market activity
try:
    df = ak.stock_market_hist(symbol="zj", period="daily", start_date="20250901", end_date="20260515")
    print("=== stock_market_hist ===")
    print(df.tail(20).to_string())
except Exception as e:
    print(f"market_hist error: {e}")