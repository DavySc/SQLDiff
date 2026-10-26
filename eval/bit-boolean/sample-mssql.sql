SET NOCOUNT ON; IF DB_ID('bitbool') IS NOT NULL BEGIN ALTER DATABASE bitbool SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE bitbool; END; CREATE DATABASE bitbool;
GO
USE bitbool;
GO
CREATE TABLE dbo.Flags (
    Id int NOT NULL PRIMARY KEY,
    IsActive bit NOT NULL DEFAULT 1,
    IsDeleted bit NULL CONSTRAINT DF_Del DEFAULT ((0)),
    Score int NULL,
    CONSTRAINT CK_Act CHECK (IsActive IN (0, 1))
);
GO
CREATE VIEW dbo.V1 AS SELECT Id, IsActive FROM dbo.Flags WHERE IsActive = 1;
GO
CREATE VIEW dbo.V2 AS SELECT Id, CASE WHEN IsActive = 1 AND IsDeleted = 0 THEN 1 ELSE 0 END AS live FROM dbo.Flags;
GO
CREATE VIEW dbo.V3 AS SELECT Id, ISNULL(IsDeleted, 0) AS d, COALESCE(IsDeleted, IsActive) AS e FROM dbo.Flags;
GO
CREATE VIEW dbo.V4 AS SELECT Id, CAST(IsActive AS int) AS a, CONVERT(int, IsDeleted) AS b, CAST(1 AS bit) AS one FROM dbo.Flags;
GO
CREATE VIEW dbo.V6 AS SELECT Id FROM dbo.Flags WHERE IsActive <> 0 OR IsDeleted != 1;
GO
CREATE VIEW dbo.V7 AS SELECT Id, ~IsActive AS na, IsActive & IsDeleted AS x, IsActive | 1 AS y, IsActive ^ IsDeleted AS z FROM dbo.Flags;
GO
CREATE VIEW dbo.V8 AS SELECT Id, IIF(Score > 2, 1, 0) AS hi, CAST(Score AS bit) AS sb, IsActive + 0 AS n FROM dbo.Flags;
GO
CREATE VIEW dbo.V9 AS SELECT Id FROM dbo.Flags WHERE IsActive = 'true' OR IsDeleted = 'FALSE' OR IsActive = '1';
GO
CREATE VIEW dbo.V10 AS SELECT MAX(CAST(IsActive AS tinyint)) AS m, MIN(Id) AS i FROM dbo.Flags WHERE IsActive IN (1) ;
GO
CREATE VIEW dbo.V11 AS SELECT a.Id FROM dbo.Flags a JOIN dbo.Flags b ON a.IsActive = b.IsDeleted;
GO
CREATE VIEW dbo.V12 AS SELECT Id, IsActive FROM dbo.Flags WHERE IsActive = 1 AND Score = 1;
GO
CREATE VIEW dbo.V13 AS SELECT Id, CAST(CASE WHEN Score > 0 THEN 1 ELSE 0 END AS bit) AS pos FROM dbo.Flags;
GO
CREATE VIEW dbo.V14 AS SELECT Id, CASE WHEN Score > 1 THEN IsActive ELSE 0 END AS g, IIF(Score > 2, 1, IsDeleted) AS h FROM dbo.Flags;
GO
CREATE VIEW dbo.W AS SELECT Id, IsActive & IsDeleted AS a, IsActive | IsDeleted AS o, IsActive ^ IsDeleted AS x, IsActive ^ 1 AS x1, IsActive & 3 AS a3, ~IsActive AS n, IsActive | 2 AS o2 FROM dbo.Flags;
GO
CREATE FUNCTION dbo.F1 (@x int) RETURNS bit AS
BEGIN
    IF @x > 0 RETURN 1;
    RETURN 0;
END
GO
CREATE FUNCTION dbo.F2 (@b bit) RETURNS int AS
BEGIN
    RETURN CASE WHEN @b = 1 THEN 10 ELSE 20 END;
END
GO
CREATE FUNCTION dbo.F4 (@x int) RETURNS bit AS
BEGIN
    DECLARE @r bit;
    SET @r = @x;
    RETURN @r;
END
GO
CREATE FUNCTION dbo.F5 (@b bit) RETURNS bit AS
BEGIN
    IF @b = 1 RETURN 0;
    RETURN @b;
END
GO
insert into dbo.Flags values (1,1,0,5),(2,0,null,0),(3,1,1,null),(4,0,0,-2);
select 'v1', * from dbo.v1 order by 2;
select 'v2', * from dbo.v2 order by 2;
select 'v3', * from dbo.v3 order by 2;
select 'v4', * from dbo.v4 order by 2;
select 'v6', * from dbo.v6 order by 2;
select 'v7', * from dbo.v7 order by 2;
select 'v8', * from dbo.v8 order by 2;
select 'v9', * from dbo.v9 order by 2;
select 'v11', * from dbo.v11 order by 2;
select 'v12', * from dbo.v12 order by 2;
select 'v13', * from dbo.v13 order by 2;
select 'v14', * from dbo.v14 order by 2;
select 'w', * from dbo.w order by 2;
select 'v10', * from dbo.v10;
select 'f', dbo.f1(3), dbo.f1(0), dbo.f2(1), dbo.f2(0), dbo.f2(null), dbo.f4(0), dbo.f4(7), dbo.f4(null), dbo.f5(1), dbo.f5(0);
GO
