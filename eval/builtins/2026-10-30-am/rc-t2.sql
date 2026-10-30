SET NOCOUNT ON;
CREATE TABLE dbo.R (Id int PRIMARY KEY, V int);
CREATE TABLE dbo.L (Id int IDENTITY PRIMARY KEY, Note varchar(20));
INSERT INTO dbo.R VALUES (1,1),(2,2),(3,3);
GO
CREATE VIEW dbo.RV AS SELECT Id, V FROM dbo.R;
GO
CREATE TRIGGER dbo.RV_io ON dbo.RV INSTEAD OF UPDATE AS BEGIN INSERT INTO dbo.L (Note) VALUES ('a'),('b'),('c'),('d'),('e'); END;
GO
UPDATE dbo.R SET V = V WHERE Id <= 2;
IF @@ROWCOUNT = 2 SELECT 'if-reads-prev' ELSE SELECT 'if-other';
UPDATE dbo.RV SET V = V WHERE Id <= 2; SELECT 'instead-of', @@ROWCOUNT;
UPDATE dbo.R SET V = V WHERE Id <= 2;
-- comment between
IF @@ROWCOUNT > 0 SELECT 'gt0';
GO
DROP VIEW dbo.RV; DROP TABLE dbo.R; DROP TABLE dbo.L;
