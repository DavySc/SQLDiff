import subprocess,sys
for l in open(sys.argv[1]):
    p=l.split(); k=p[0]; h=p[1] if len(p)>1 else ''
    sql=f"do $$ declare g geography; begin g := ST_GeomFromWKB('\\x{h}'::bytea,4326)::geography; raise notice '%|ok|%|%', '{k}', ST_IsValid(g::geometry), upper(encode(ST_AsBinary(g),'hex'))='{h.upper()}'; exception when others then raise notice '%|ERR|%', '{k}', sqlerrm; end $$;"
    r=subprocess.run(['/tmp/claude-0/w/pg.sh','-c',sql],capture_output=True,text=True)
    print([x for x in (r.stderr+r.stdout).split('\n') if '|' in x][-1].replace('NOTICE:  ',''))
