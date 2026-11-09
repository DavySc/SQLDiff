diffset () 
{ 
    name=$1;
    shift;
    rm -rf dump/$name mut/$name mut/$name.err;
    mkdir -p mut/$name;
    for f in "$@";
    do
        rm -rf /root/.local/share/sqldiff/cache;
        b=mut/$name/$(basename $f);
        python3 gen/mutate.py $f $b ${EVERY:-7};
        SQLDIFF_DATALOG_DUMP=dump/$name timeout 600 $B diff $f $b --dialect tsql > /dev/null 2>> mut/$name.err;
    done;
    echo "$name: $(wc -l < dump/$name/rust_time.tsv) sweep calls, $(wc -l < dump/$name/edit.facts) edits, $(wc -l < dump/$name/unit.facts) units"
}
gen () 
{ 
    name=$1;
    shift;
    rm -rf /root/.local/share/sqldiff/cache;
    python3 gen/chain.py "$@" in/$name;
    rm -rf dump/$name;
    SQLDIFF_DATALOG_DUMP=dump/$name timeout 600 $B diff in/$name.a.sql in/$name.b.sql --dialect tsql > /dev/null 2>&1;
    echo "$name: $(wc -l < dump/$name/edit.facts) edits, $(wc -l < dump/$name/unit.facts) units"
}
