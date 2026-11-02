#!/bin/bash
# usage: runms.sh file(name hex) -> name result
while read -r k h; do echo "BEGIN TRY DECLARE @g geography=geography::STGeomFromWKB(0x$h,4326); SELECT '$k' AS k, 'ok' AS r, @g.STIsValid() AS v, CASE WHEN @g.STAsBinary()=0x$h THEN 1 ELSE 0 END AS same; END TRY BEGIN CATCH SELECT '$k', 'ERR '+CAST(ERROR_NUMBER() AS varchar(10)), NULL, NULL; END CATCH"; echo GO; done < $1 | ${MS:-./ms.sh} -i /dev/stdin 2>&1 | grep -E '^[A-Za-z_0-9-]+ (ok|ERR)'
