import akshare as ak

fns = [x for x in dir(ak) if 'stock' in x.lower()]
keywords = ['market', 'breadth', 'activity', '涨跌', '上涨', '下跌', '情绪', 'limit', 'zt', 'zd']
results = []
for f in sorted(fns):
    for kw in keywords:
        if kw.lower() in f.lower() or kw in f:
            results.append(f)
            break

for r in results:
    print(r)