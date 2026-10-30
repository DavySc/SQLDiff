"""Compare iso126-mssql.out with iso126-pg.out: python3 iso126-cmp.py"""
import re

cases = open('iso126-cases.txt').read().split('\n')[:-1]
ms = dict(l.split('|', 1) for l in open('iso126-mssql.out').read().split('\n') if l)
pg = dict(l.split('|', 1) for l in open('iso126-pg.out').read().split('\n') if l)
iso = re.compile(r'^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]{0,7})?$')
kinds = {}
for i, c in enumerate(cases):
    a, b = ms[str(i)], pg[str(i)]
    if a == b:
        k = 'same'
    elif iso.match(c) and a != 'NULL' and b != 'NULL' and a[:-1] == b[:-1]:
        k = 'only the 7th fraction digit (ASM-FRACTIONAL-SECONDS)'
    elif iso.match(c):
        k = 'DIFFERENT in the ISO form'
    else:
        k = 'SQL Server reads a form outside ASM-ISO126-VALUES'
    kinds.setdefault(k, []).append((c, a, b))
for k, v in kinds.items():
    print(len(v), k)
