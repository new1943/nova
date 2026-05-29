import requests
import re

# Try to get the page source and find JavaScript files that might handle the chart
r = requests.get("https://legulegu.com/stockdata/market-activity", timeout=10)

# Look for script src tags
scripts = re.findall(r'<script[^>]*src=["\']([^"\']+)["\']', r.text)
print("Scripts found:")
for s in scripts:
    print(s)

# Look for inline scripts with API calls
inline = re.findall(r'<script[^>]*>(.*?)</script>', r.text, re.DOTALL)
for idx, s in enumerate(inline):
    if 'activity' in s.lower() or 'market' in s.lower() or 'chart' in s.lower():
        print(f"\n=== Inline script {idx} ===")
        print(s[:500])

# Look for chart data URLs
chart_patterns = re.findall(r'(https?://[^"\'>\s]+legulegu[^"\'>\s]*)', r.text)
print("\n\nLegulegu URLs:")
for u in chart_patterns[:20]:
    print(u)