# Generates the money probes for SQL Server (money) and PostgreSQL (numeric(19,4)).
# usage: gen.py tsql|pg  > file.sql ; outputs one line per case: label|value
import sys
d=sys.argv[1]
vals=['0.0000','0.0001','0.0005','0.5000','1.0000','2.5000','0.1235','1.2345','-0.0001','-2.5000','-1.5000','1234.5678','922337203685477.5807','-922337203685477.5808','100.0000','0.3333','7.0000']
ints=['0','1','2','3','-3','7','32767','100000']
def m(v): return f"CAST('{v}' AS money)" if d=='tsql' else f"CAST('{v}' AS numeric(19,4))"
def i(v): return f"CAST({v} AS int)"
def sel(label,expr):
    if d=='tsql':
        # money results print with 4 decimals (style 2); CAST(money AS varchar) keeps 2.
        conv = f"CONVERT(varchar(60), {expr}, 2)" if label.split()[0] in MONEY else f"CAST({expr} AS varchar(60))"
        return f"BEGIN TRY SELECT '{label}', {conv}; END TRY BEGIN CATCH SELECT '{label}', 'ERR'; END CATCH"
    return f"DO $$ BEGIN RAISE NOTICE '%|%','{label}', ({expr})::text; EXCEPTION WHEN OTHERS THEN RAISE NOTICE '%|%','{label}','ERR'; END $$;"
out=[]
MONEY={'add','sub','mul','div','mulint','intmul','addint','divint','abs','round','neg'}
n=0
def add(label,expr): out.append(sel(label,expr))
rnd = (lambda e,s: f"CAST({e} AS decimal(38,{s}))") 
for a in vals:
    for b in vals:
        if (len(a)*7+len(b)*3+ord(a[-1])+ord(b[-1]))%3==0:
            add(f'add {a} {b}',f"{m(a)} + {m(b)}")
            add(f'sub {a} {b}',f"{m(a)} - {m(b)}")
            add(f'mul {a} {b}',f"{m(a)} * {m(b)}")
            if b not in('0.0000',): add(f'div {a} {b}',f"{m(a)} / {m(b)}")
            add(f'eq {a} {b}',f"CASE WHEN {m(a)} < {m(b)} THEN 'lt' WHEN {m(a)} = {m(b)} THEN 'eq' ELSE 'gt' END")
    for k in ints:
        add(f'mulint {a} {k}',f"{m(a)} * {i(k)}")
        add(f'intmul {a} {k}',f"{i(k)} * {m(a)}")
        add(f'addint {a} {k}',f"{m(a)} + {i(k)}")
        if k!='0': add(f'divint {a} {k}',f"{m(a)} / {i(k)}")
    add(f'toint {a}',f"CAST({m(a)} AS int)")
    add(f'tobigint {a}',f"CAST({m(a)} AS bigint)")
    add(f'todec {a}',f"CAST({m(a)} AS decimal(10,2))")
    add(f'todec0 {a}',f"CAST({m(a)} AS decimal(18,0))")
    add(f'todec4 {a}',f"CAST({m(a)} AS decimal(20,4))")
    add(f'tovarchar {a}',f"CAST({m(a)} AS varchar(30))")
    add(f'tofloat {a}',f"CAST({m(a)} AS float)")
    add(f'abs {a}',f"ABS({m(a)})")
    add(f'round {a}',f"ROUND({m(a)}, 2)")
    add(f'neg {a}',f"-{m(a)}")
if d=='tsql': print("SET NOCOUNT ON;"); 
print("\n".join(out))
