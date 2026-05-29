import akshare as ak
import json

# Let's see what market-related functions exist
fns = [x for x in dir(ak) if 'stock' in x.lower()]
for f in sorted(fns):
    print(f)