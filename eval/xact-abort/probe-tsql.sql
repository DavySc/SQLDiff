-- Probe 2026-11-02, SQL Server 2022: after which errors does a procedure (XACT_ABORT OFF, no
-- open transaction, SET NOCOUNT ON) go on with the next statement, and what do the failed
-- statements leave behind? One table per case; step column records which statements ran.
SET NOCOUNT ON;
IF OBJECT_ID('dbo.t') IS NOT NULL DROP TABLE dbo.t;
IF OBJECT_ID('dbo.pk') IS NOT NULL DROP TABLE dbo.pk;
IF OBJECT_ID('dbo.log') IS NOT NULL DROP TABLE dbo.log;
CREATE TABLE dbo.pk (id int NOT NULL PRIMARY KEY);
CREATE TABLE dbo.t (id int NOT NULL PRIMARY KEY, n int NOT NULL, s varchar(3) NULL, c int NULL CHECK (c > 0), fk int NULL REFERENCES dbo.pk(id));
CREATE TABLE dbo.log (case_name varchar(40), step int);
GO
IF OBJECT_ID('dbo.cases') IS NOT NULL DROP PROCEDURE dbo.cases;
GO
CREATE PROCEDURE dbo.cases @which varchar(40) AS
BEGIN
  SET NOCOUNT ON;
  INSERT dbo.log VALUES (@which, 1);
  IF @which = 'unique'    INSERT dbo.t (id, n) VALUES (1, 1), (1, 2);
  IF @which = 'notnull'   INSERT dbo.t (id, n) VALUES (2, NULL);
  IF @which = 'check'     INSERT dbo.t (id, n, c) VALUES (3, 1, -1);
  IF @which = 'fk'        INSERT dbo.t (id, n, fk) VALUES (4, 1, 99);
  IF @which = 'convert'   INSERT dbo.t (id, n) VALUES (5, 'abc');
  IF @which = 'overflow'  INSERT dbo.t (id, n) VALUES (6, 3000000000);
  IF @which = 'truncate'  INSERT dbo.t (id, n, s) VALUES (7, 1, 'toolong');
  IF @which = 'divzero'   INSERT dbo.t (id, n) VALUES (8, 1/0);
  IF @which = 'badobject' INSERT dbo.nosuchtable (id) VALUES (9);
  IF @which = 'throw'     THROW 50001, 'x', 1;
  IF @which = 'raiserror' RAISERROR('x', 16, 1);
  IF @which = 'execbad'   EXEC dbo.nosuchproc;
  IF @which = 'updconv'   UPDATE dbo.t SET n = CAST('abc' AS int);
  IF @which = 'delfk'     DELETE dbo.pk;
  INSERT dbo.log VALUES (@which, 2);
END
GO
IF OBJECT_ID('dbo.outer_p') IS NOT NULL DROP PROCEDURE dbo.outer_p;
GO
CREATE PROCEDURE dbo.outer_p @which varchar(40) AS
BEGIN
  SET NOCOUNT ON;
  INSERT dbo.log VALUES ('outer-' + @which, 1);
  EXEC dbo.cases @which;
  INSERT dbo.log VALUES ('outer-' + @which, 3);
END
GO
INSERT dbo.pk VALUES (1);
INSERT dbo.t (id, n, fk) VALUES (100, 1, 1);
GO
EXEC dbo.outer_p 'unique';
GO
EXEC dbo.outer_p 'notnull';
GO
EXEC dbo.outer_p 'check';
GO
EXEC dbo.outer_p 'fk';
GO
EXEC dbo.outer_p 'convert';
GO
EXEC dbo.outer_p 'overflow';
GO
EXEC dbo.outer_p 'truncate';
GO
EXEC dbo.outer_p 'divzero';
GO
EXEC dbo.outer_p 'badobject';
GO
EXEC dbo.outer_p 'throw';
GO
EXEC dbo.outer_p 'raiserror';
GO
EXEC dbo.outer_p 'execbad';
GO
EXEC dbo.outer_p 'updconv';
GO
EXEC dbo.outer_p 'delfk';
GO
GO
SELECT case_name, string_agg(CAST(step AS varchar), ',') WITHIN GROUP (ORDER BY step) AS steps FROM dbo.log GROUP BY case_name ORDER BY case_name;
SELECT 'rows in t', count(*) FROM dbo.t;
