SET NOCOUNT ON;
GO
DECLARE @x nvarchar(max)=N'{"R":"a","D":"2016-01-02T10:11:12.1234567","Tags":[1,2]}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '0|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '0|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"a","D":"2016-01-02T10:11:12.1234567","Tags":[1,2]}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '0|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '0|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"a","D":"2016-01-02T10:11:12.1234567","Tags":[1,2]}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '0|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '0|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"a","D":"2016-01-02T10:11:12.1234567","Tags":[1,2]}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '0|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '0|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"a","R":"b","Tags":[1],"Tags":[2]}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '1|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '1|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"a","R":"b","Tags":[1],"Tags":[2]}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '1|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '1|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"a","R":"b","Tags":[1],"Tags":[2]}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '1|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '1|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"a","R":"b","Tags":[1],"Tags":[2]}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '1|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '1|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"Tags":[1],"Tags":"s"}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '2|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '2|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"Tags":[1],"Tags":"s"}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '2|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '2|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"Tags":[1],"Tags":"s"}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '2|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '2|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"Tags":[1],"Tags":"s"}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '2|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '2|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"Tags":"s","Tags":[1]}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '3|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '3|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"Tags":"s","Tags":[1]}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '3|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '3|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"Tags":"s","Tags":[1]}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '3|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '3|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"Tags":"s","Tags":[1]}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '3|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '3|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"\u0000x"}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '4|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '4|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"\u0000x"}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '4|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '4|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"\u0000x"}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '4|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '4|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"\u0000x"}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '4|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '4|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"\ud800"}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '5|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '5|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"\ud800"}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '5|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '5|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"\ud800"}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '5|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '5|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"\ud800"}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '5|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '5|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"😀"}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '6|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '6|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"😀"}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '6|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '6|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"😀"}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '6|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '6|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"😀"}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '6|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '6|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"a\"b\\c\/d\n\t"}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '7|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '7|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"a\"b\\c\/d\n\t"}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '7|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '7|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"a\"b\\c\/d\n\t"}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '7|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '7|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"a\"b\\c\/d\n\t"}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '7|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '7|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":12.50e1,"Tags":{ "a" : [ 1 , 2 ] }}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '8|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '8|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":12.50e1,"Tags":{ "a" : [ 1 , 2 ] }}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '8|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '8|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":12.50e1,"Tags":{ "a" : [ 1 , 2 ] }}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '8|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '8|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":12.50e1,"Tags":{ "a" : [ 1 , 2 ] }}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '8|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '8|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":true,"Tags":null}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '9|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '9|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":true,"Tags":null}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '9|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '9|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":true,"Tags":null}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '9|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '9|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":true,"Tags":null}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '9|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '9|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":null}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '10|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '10|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":null}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '10|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '10|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":null}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '10|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '10|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":null}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '10|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '10|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":[1],"Tags":{}}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '11|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '11|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":[1],"Tags":{}}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '11|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '11|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":[1],"Tags":{}}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '11|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '11|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":[1],"Tags":{}}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '11|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '11|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"r":"lower","tags":[1]}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '12|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '12|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"r":"lower","tags":[1]}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '12|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '12|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"r":"lower","tags":[1]}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '12|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '12|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"r":"lower","tags":[1]}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '12|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '12|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'[1,2]'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '13|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '13|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'[1,2]'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '13|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '13|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'[1,2]'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '13|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '13|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'[1,2]'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '13|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '13|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'1'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '14|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '14|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'1'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '14|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '14|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'1'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '14|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '14|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'1'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '14|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '14|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'"str"'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '15|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '15|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'"str"'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '15|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '15|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'"str"'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '15|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '15|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'"str"'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '15|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '15|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'null'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '16|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '16|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'null'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '16|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '16|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'null'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '16|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '16|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'null'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '16|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '16|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '17|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '17|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '17|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '17|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '17|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '17|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '17|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '17|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"Tags":[1]}trailing'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '18|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '18|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"Tags":[1]}trailing'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '18|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '18|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"Tags":[1]}trailing'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '18|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '18|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"Tags":[1]}trailing'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '18|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '18|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"Tags":[1]} '; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '19|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '19|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"Tags":[1]} '; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '19|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '19|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"Tags":[1]} '; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '19|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '19|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"Tags":[1]} '; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '19|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '19|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N' {"Tags":[1]}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '20|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '20|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N' {"Tags":[1]}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '20|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '20|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N' {"Tags":[1]}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '20|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '20|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N' {"Tags":[1]}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '20|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '20|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"Tags":[1],}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '21|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '21|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"Tags":[1],}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '21|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '21|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"Tags":[1],}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '21|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '21|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"Tags":[1],}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '21|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '21|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{''Tags'':[1]}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '22|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '22|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{''Tags'':[1]}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '22|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '22|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{''Tags'':[1]}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '22|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '22|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{''Tags'':[1]}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '22|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '22|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"Tags":[1]'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '23|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '23|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"Tags":[1]'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '23|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '23|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"Tags":[1]'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '23|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '23|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"Tags":[1]'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '23|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '23|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"a":{"Tags":[9]},"Tags":[1]}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '24|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '24|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"a":{"Tags":[9]},"Tags":[1]}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '24|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '24|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"a":{"Tags":[9]},"Tags":[1]}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '24|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '24|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"a":{"Tags":[9]},"Tags":[1]}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '24|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '24|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"é"}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '25|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '25|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"é"}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '25|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '25|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"é"}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '25|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '25|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"é"}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '25|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '25|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"é"}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '26|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '26|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"é"}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '26|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '26|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"é"}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '26|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '26|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"é"}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '26|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '26|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":-0,"Tags":[ ]}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '27|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '27|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":-0,"Tags":[ ]}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '27|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '27|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":-0,"Tags":[ ]}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '27|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '27|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":-0,"Tags":[ ]}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '27|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '27|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":1E400}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '28|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '28|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":1E400}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '28|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '28|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":1E400}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '28|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '28|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":1E400}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '28|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '28|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":01}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '29|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '29|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":01}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '29|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '29|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":01}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '29|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '29|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":01}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '29|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '29|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":.5}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '30|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '30|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":.5}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '30|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '30|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":.5}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '30|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '30|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":.5}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '30|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '30|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"tab	inside"}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),ISJSON(@x)); SELECT '31|isjson|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '31|isjson|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"tab	inside"}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_QUERY(@x,N'$.Tags'); SELECT '31|q|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '31|q|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"tab	inside"}'; BEGIN TRY DECLARE @r nvarchar(max)=JSON_VALUE(@x,N'$.R'); SELECT '31|v|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '31|v|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
DECLARE @x nvarchar(max)=N'{"R":"tab	inside"}'; BEGIN TRY DECLARE @r nvarchar(max)=CONVERT(nvarchar(max),TRY_CONVERT(datetime2(7),JSON_VALUE(@x,N'$.D'),126),126); SELECT '31|d|'+ISNULL(CONVERT(varchar(max),CAST(@r AS varbinary(max)),2),'NULL'); END TRY BEGIN CATCH SELECT '31|d|ERR '+CONVERT(varchar(10),ERROR_NUMBER()); END CATCH
GO
