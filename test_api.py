import requests
import json

# Try various legulegu API endpoints
endpoints = [
    "https://legulegu.com/api/stockdata/market-activity-list",
    "https://legulegu.com/api/stockdata/get-market-activity-list",
    "https://legulegu.com/api/market-activity/list",
    "https://legulegu.com/stockdata/market-activity-history",
]

for url in endpoints:
    try:
        r = requests.get(url, timeout=5)
        print(f"=== {url} ===")
        print(f"Status: {r.status_code}")
        print(f"Content: {r.text[:500]}")
        print()
    except Exception as e:
        print(f"{url}: {e}")
        print()

# Try to get the page source to find API endpoints
try:
    r = requests.get("https://legulegu.com/stockdata/market-activity", timeout=10)
    # Look for API patterns
    import re
    patterns = re.findall(r'(https?://[^"\'>\s]+api[^"\'>\s]*)', r.text)
    for p in patterns[:20]:
        print(f"Found: {p}")
except Exception as e:
    print(f"Page source error: {e}")