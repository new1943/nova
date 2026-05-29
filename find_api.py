import requests
import re

# Try to get the page source and find API patterns
r = requests.get("https://legulegu.com/stockdata/market-activity", timeout=10)

# Look for axios/fetch API patterns
patterns = [
    r"axios\.[get|post]\(['\"]([^'\"]+)['\"]",
    r"url:\s*['\"]([^'\"]+)['\"]",
    r"api['\"]:\s*['\"]([^'\"]+)['\"]",
    r"API_URL['\"]:\s*['\"]([^'\"]+)['\"]",
    r"baseURL['\"]:\s*['\"]([^'\"]+)['\"]",
    r"fetch\(['\"]([^'\"]+)['\"]",
]

found = set()
for p in patterns:
    matches = re.findall(p, r.text)
    for m in matches:
        found.add(m)

print("Found API patterns:")
for f in sorted(found):
    print(f)

print("\n\nLooking for market-activity related:")
for line in r.text.split('\n'):
    if 'market' in line.lower() or 'activity' in line.lower() or 'api' in line.lower():
        if any(x in line for x in ['url', 'Url', 'URL', 'api', 'Api', 'fetch', 'axios', 'get', 'post']):
            print(line.strip()[:200])