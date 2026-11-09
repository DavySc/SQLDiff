# mutate.py SRC DST EVERY: change every EVERY-th integer literal outside comments and strings (N -> N+1).
import re,sys
src,dst,every=sys.argv[1],sys.argv[2],int(sys.argv[3])
t=open(src,encoding='utf-8-sig',errors='replace').read()
out=[];i=0;k=0;n=len(t)
while i<n:
    c=t[i]
    if t.startswith('--',i):
        j=t.find('\n',i); j=n if j<0 else j; out.append(t[i:j]); i=j; continue
    if t.startswith('/*',i):
        j=t.find('*/',i+2); j=n if j<0 else j+2; out.append(t[i:j]); i=j; continue
    if c=="'":
        j=i+1
        while j<n:
            if t[j]=="'":
                if j+1<n and t[j+1]=="'": j+=2; continue
                break
            j+=1
        out.append(t[i:j+1]); i=j+1; continue
    m=re.match(r'[A-Za-z_@#\[][\w@#$\]]*',t[i:]) 
    if m: out.append(m.group()); i+=m.end(); continue
    m=re.match(r'\d+(?![\w.])',t[i:])
    if m:
        k+=1
        out.append(str(int(m.group())+1) if k%every==0 else m.group()); i+=m.end(); continue
    out.append(c); i+=1
open(dst,'w').write(''.join(out))
