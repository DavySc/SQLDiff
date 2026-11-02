-- A keyed write that SETs an indexed (key) column of the row just fetched.
-- A FAST_FORWARD cursor over the table (no ORDER BY, so it scans the key's
-- index) can meet the moved row again; a snapshot (PostgreSQL) cannot.
SET NOCOUNT ON;
CREATE TABLE dbo.T (Id int NOT NULL PRIMARY KEY, V int NOT NULL);
INSERT dbo.T VALUES (1,0),(2,0),(3,0);
DECLARE @id int, @v int, @seen nvarchar(200) = N'';
DECLARE c CURSOR FAST_FORWARD READ_ONLY FOR SELECT Id, V FROM dbo.T ORDER BY Id;
OPEN c; FETCH NEXT FROM c INTO @id, @v;
WHILE @@FETCH_STATUS = 0
BEGIN
  SET @seen += CAST(@id AS nvarchar(10)) + N',';
  IF @id < 100
  BEGIN
    UPDATE dbo.T SET Id = Id + 100 WHERE Id = @id;   -- moves the row in the key's index
  END
  FETCH NEXT FROM c INTO @id, @v;
END; CLOSE c; DEALLOCATE c;
SELECT 'key-moved' AS probe, @seen AS visited;   -- snapshot: 1,2,3,
DROP TABLE dbo.T;
-- Same with a non-indexed column: the row does not move.
CREATE TABLE dbo.T (Id int NOT NULL PRIMARY KEY, V int NOT NULL);
INSERT dbo.T VALUES (1,0),(2,0),(3,0);
SET @seen = N'';
DECLARE c2 CURSOR FAST_FORWARD READ_ONLY FOR SELECT Id, V FROM dbo.T ORDER BY Id;
OPEN c2; FETCH NEXT FROM c2 INTO @id, @v;
WHILE @@FETCH_STATUS = 0
BEGIN
  SET @seen += CAST(@id AS nvarchar(10)) + N',';
  UPDATE dbo.T SET V = V + 1 WHERE Id = @id;
  FETCH NEXT FROM c2 INTO @id, @v;
END; CLOSE c2; DEALLOCATE c2;
SELECT 'non-key' AS probe, @seen AS visited;     -- 1,2,3,
DROP TABLE dbo.T;
