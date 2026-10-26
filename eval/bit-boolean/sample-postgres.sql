drop schema if exists bitbool cascade; create schema bitbool; set search_path=bitbool;
-- [sqldiff: needs a decision] pg.bit-boolean
CREATE TABLE Flags (
    Id integer NOT NULL PRIMARY KEY,
    IsActive boolean NOT NULL DEFAULT TRUE,
    IsDeleted boolean NULL CONSTRAINT DF_Del DEFAULT FALSE,
    Score integer NULL,
    CONSTRAINT CK_Act CHECK (IsActive IN (FALSE, TRUE))
);

-- [sqldiff: under assumptions] ASM-DBO-SCHEMA
CREATE OR REPLACE VIEW V1 AS
SELECT Id, IsActive FROM Flags WHERE IsActive = TRUE;

-- [sqldiff: under assumptions] ASM-DBO-SCHEMA
CREATE OR REPLACE VIEW V2 AS
SELECT Id, CASE WHEN IsActive = TRUE AND IsDeleted = FALSE THEN 1 ELSE 0 END AS live FROM Flags;

-- [sqldiff: under assumptions] ASM-DBO-SCHEMA, ASM-ISNULL-TYPE
CREATE OR REPLACE VIEW V3 AS
SELECT Id, COALESCE(IsDeleted, FALSE) AS d, COALESCE(IsDeleted, IsActive) AS e FROM Flags;

-- [sqldiff: not proven] equiv.unproven
CREATE OR REPLACE VIEW V4 AS
SELECT Id, CAST(IsActive AS integer) AS a, CAST(IsDeleted AS integer) AS b, TRUE AS one FROM Flags;

-- [sqldiff: under assumptions] ASM-DBO-SCHEMA
CREATE OR REPLACE VIEW V6 AS
SELECT Id FROM Flags WHERE IsActive <> FALSE OR IsDeleted != TRUE;

-- [sqldiff: not proven] equiv.unsupported
CREATE OR REPLACE VIEW V7 AS
SELECT Id, (NOT IsActive) AS na, (IsActive::integer & IsDeleted::integer)::boolean AS x, IsActive::integer | 1 AS y, (IsActive <> IsDeleted) AS z FROM Flags;

-- [sqldiff: needs a decision] pg.bit-boolean
CREATE OR REPLACE VIEW V8 AS
SELECT Id, (CASE WHEN Score > 2 THEN 1 ELSE 0 END) AS hi, (Score <> 0) AS sb, IsActive::integer + 0 AS n FROM Flags;

-- [sqldiff: under assumptions] ASM-DBO-SCHEMA, ASM-CI-COLLATION
CREATE OR REPLACE VIEW V9 AS
SELECT Id FROM Flags WHERE IsActive = 'true' OR IsDeleted = 'FALSE' OR IsActive = '1';

-- [sqldiff: not proven] equiv.unproven
CREATE OR REPLACE VIEW V10 AS
SELECT MAX(CAST(IsActive::integer AS smallint)) AS m, MIN(Id) AS i FROM Flags WHERE IsActive IN (TRUE);

-- [sqldiff: under assumptions] ASM-DBO-SCHEMA
CREATE OR REPLACE VIEW V11 AS
SELECT a.Id FROM Flags a JOIN Flags b ON a.IsActive = b.IsDeleted;

-- [sqldiff: under assumptions] ASM-DBO-SCHEMA
CREATE OR REPLACE VIEW V12 AS
SELECT Id, IsActive FROM Flags WHERE IsActive = TRUE AND Score = 1;

-- [sqldiff: needs a decision] pg.bit-boolean
CREATE OR REPLACE VIEW V13 AS
SELECT Id, CAST(CASE WHEN Score > 0 THEN 1 ELSE 0 END AS boolean) AS pos FROM Flags;

-- [sqldiff: under assumptions] ASM-DBO-SCHEMA
CREATE OR REPLACE VIEW V14 AS
SELECT Id, CASE WHEN Score > 1 THEN IsActive ELSE FALSE END AS g, (CASE WHEN Score > 2 THEN TRUE ELSE IsDeleted END) AS h FROM Flags;

-- [sqldiff: not proven] equiv.unsupported
CREATE OR REPLACE VIEW W AS
SELECT Id, (IsActive::integer & IsDeleted::integer)::boolean AS a, (IsActive::integer | IsDeleted::integer)::boolean AS o, (IsActive <> IsDeleted) AS x, IsActive::integer # 1 AS x1, IsActive::integer & 3 AS a3, (NOT IsActive) AS n, IsActive::integer | 2 AS o2 FROM Flags;

-- [sqldiff: needs a decision] pg.bit-boolean
CREATE OR REPLACE FUNCTION F1(p_x integer) RETURNS boolean
LANGUAGE plpgsql
AS $$
BEGIN
  IF p_x > 0 THEN
    RETURN TRUE;
  END IF;
  RETURN FALSE;
END;
$$;

-- [sqldiff: needs a decision] pg.bit-boolean
CREATE OR REPLACE FUNCTION F2(p_b boolean) RETURNS integer
LANGUAGE plpgsql
AS $$
BEGIN
  RETURN CASE WHEN p_b = TRUE THEN 10 ELSE 20 END;
END;
$$;

-- [sqldiff: needs a decision] pg.bit-boolean
CREATE OR REPLACE FUNCTION F4(p_x integer) RETURNS boolean
LANGUAGE plpgsql
AS $$
DECLARE
  -- [sqldiff: needs a decision] pg.bit-boolean
  v_r boolean;
BEGIN
  -- DECLARE @r: moved to the DECLARE section
  v_r := (p_x <> 0);
  RETURN v_r;
END;
$$;

-- [sqldiff: needs a decision] pg.bit-boolean
CREATE OR REPLACE FUNCTION F5(p_b boolean) RETURNS boolean
LANGUAGE plpgsql
AS $$
BEGIN
  IF p_b = TRUE THEN
    RETURN FALSE;
  END IF;
  RETURN p_b;
END;
$$;

insert into Flags values (1,true,false,5),(2,false,null,0),(3,true,true,null),(4,false,false,-2);
select 'v1', * from v1 order by 2;
select 'v2', * from v2 order by 2;
select 'v3', * from v3 order by 2;
select 'v4', * from v4 order by 2;
select 'v6', * from v6 order by 2;
select 'v7', * from v7 order by 2;
select 'v8', * from v8 order by 2;
select 'v9', * from v9 order by 2;
select 'v11', * from v11 order by 2;
select 'v12', * from v12 order by 2;
select 'v13', * from v13 order by 2;
select 'v14', * from v14 order by 2;
select 'w', * from w order by 2;
select 'v10', * from v10;
select 'f', f1(3), f1(0), f2(true), f2(false), f2(null), f4(0), f4(7), f4(null), f5(true), f5(false);
