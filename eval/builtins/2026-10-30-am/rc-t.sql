SET NOCOUNT ON;
CREATE TABLE dbo.R (Id int PRIMARY KEY, V int);
CREATE TABLE dbo.L (Id int IDENTITY PRIMARY KEY, Note varchar(20));
INSERT INTO dbo.R VALUES (1,1),(2,2),(3,3);
GO
CREATE TRIGGER dbo.R_after ON dbo.R AFTER UPDATE AS BEGIN INSERT INTO dbo.L (Note) VALUES ('a'),('b'),('c'),('d'),('e'); END;
GO
DECLARE @x int = -1;
SELECT TOP 1 @x = V FROM dbo.R WHERE Id > 10 ORDER BY Id; SELECT 'top1-none', @@ROWCOUNT, @x;
SELECT TOP 1 @x = V FROM dbo.R ORDER BY Id; SELECT 'top1-one', @@ROWCOUNT, @x;
SELECT @x = V FROM dbo.R; SELECT 'all', @@ROWCOUNT, @x;
UPDATE dbo.R SET V = V WHERE Id <= 2; SELECT 'update-trigger', @@ROWCOUNT;
UPDATE dbo.R SET V = V WHERE Id > 10; SELECT 'update-none-trigger', @@ROWCOUNT;
DELETE FROM dbo.L WHERE Id < 0; SELECT 'delete-none', @@ROWCOUNT;
IF 1 = 1 SELECT 'in-if', @@ROWCOUNT;
GO
DROP TABLE dbo.R; DROP TABLE dbo.L;
