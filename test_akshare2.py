import akshare as ak

# Try more functions
results = []

# 1. stock market activity
try:
    df = ak.stock_market_activity_legu()
    results.append(("legu", df))
except Exception as e:
    results.append(("legu_error", str(e)))

# 2. index spot em
try:
    df = ak.stock_zh_index_spot_em()
    results.append(("index_spot", list(df.columns)[:10]))
except Exception as e:
    results.append(("index_spot_error", str(e)))

# 3. stock board industry spot
try:
    df = ak.stock_board_industry_spot_em()
    results.append(("board_spot", list(df.columns)[:10]))
except Exception as e:
    results.append(("board_spot_error", str(e)))

# 4. search for advance/decline related functions
import akshare as ak
fns = [x for x in dir(ak) if 'market' in x.lower() or 'breadth' in x.lower() or 'activity' in x.lower() or 'advance' in x.lower() or 'decline' in x.lower()]
results.append(("matching_funcs", fns))

# 5. check what functions exist related to "涨跌"
fns2 = [x for x in dir(ak) if '涨跌' in x]
results.append(("涨跌_funcs", fns2))

# 6. check stockdata related
fns3 = [x for x in dir(ak) if 'stockdata' in x.lower() or 'stock_data' in x.lower()]
results.append(("stockdata_funcs", fns3))

for name, val in results:
    print(f"=== {name} ===")
    if isinstance(val, list):
        print(val)
    elif hasattr(val, 'to_string'):
        print(val.to_string())
    else:
        print(val)
    print()