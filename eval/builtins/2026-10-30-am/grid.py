import sys
needles=["''","'a'","'b'","'ab'","'A'","'x'","'b '","NULL"]
hays=["''","'abcab'","'aaa'","NULL","'ab '","'xAbx'"]
starts=["NULL","-5","0","1","2","3","4","5","6","100","2147483647","-2147483648"]
rows=[(n,h,s) for n in needles for h in hays for s in starts]
t=sys.argv[1]=='tsql'
if t: print("SET NOCOUNT ON;")
for i,(n,h,s) in enumerate(rows):
    x=f"CAST({n} AS varchar(10))"; y=f"CAST({h} AS varchar(10))"; st=f"CAST({s} AS {'int' if t else 'integer'})"
    e=f"CHARINDEX({x}, {y} COLLATE Latin1_General_100_CS_AS, {st})" if t else f"((strpos(substr({y}, {st}), {x}) - 1 + greatest({st}, 1)) * (strpos(substr({y}, {st}), {x}) > 0 AND length({x}) > 0)::int)"
    print(f"SELECT CONCAT('{i}|', COALESCE(CAST({e} AS varchar(20)),'NULL'));")
