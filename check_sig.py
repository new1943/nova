import akshare as ak
import inspect

# Check the signature of stock_zdhtmx_em
fn = ak.stock_zdhtmx_em
sig = inspect.signature(fn)
print(f"stock_zdhtmx_em signature: {sig}")
help(fn)
print()

# Also check stock_zt_pool_previous_em 
fn2 = ak.stock_zt_pool_previous_em
sig2 = inspect.signature(fn2)
print(f"stock_zt_pool_previous_em signature: {sig2}")

fn3 = ak.stock_zt_pool_sub_new_em
sig3 = inspect.signature(fn3)
print(f"stock_zt_pool_sub_new_em signature: {sig3}")

fn4 = ak.stock_zt_pool_zbgc_em
sig4 = inspect.signature(fn4)
print(f"stock_zt_pool_zbgc_em signature: {sig4}")