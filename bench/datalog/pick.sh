#!/bin/bash
# pick.sh SRC DST C : one instance of a dump
mkdir -p $2; for f in $1/*.facts $1/*.tsv; do awk -v c=$3 '$1==c' $f > $2/$(basename $f); done
