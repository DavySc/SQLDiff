B=r'\[(?:[^]]|]]){0,128}]'
Q=r'"(?:[^"]|""){0,128}"'
def pl(first): return r'[^].["]{%d,128}' % (1 if first else 0)
def anyp(i): return '(?:%s|%s|%s)' % (B, Q, pl(i==1))
TGT={'b': r'\[((?:[^]]|]]){0,128})]', 'q': r'"((?:[^"]|""){0,128})"'}
def tgt(kind,i):
    if kind=='p': return '(%s)' % pl(i==1)
    return TGT[kind]
def regex(k,kind):
    X={i:(tgt(kind,i) if i==k else anyp(i)) for i in (1,2,3,4)}
    return '^(?:(?:(?:%s\\.)?%s\\.)?%s\\.)?%s$' % (X[4],X[3],X[2],X[1])
def template(k):
    lit=lambda r: "'"+r.replace("'","''")+"'"
    b=f"substring(__x__, {lit(regex(k,'b'))})"
    q=f"substring(__x__, {lit(regex(k,'q'))})"
    p=f"substring(__x__, {lit(regex(k,'p'))})"
    return f"""NULLIF(COALESCE(replace({b}, ']]', ']'), replace({q}, '""', '"'), {p}), '')"""
if __name__=='__main__':
    import sys
    for k in (1,2,3,4): print(len(template(k)))
