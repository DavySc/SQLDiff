-- A non-STATIC cursor whose loop changes only the row just fetched (unique key):
-- the rows the loop visits are the rows a snapshot would give. Contrast: a write
-- that reaches rows not yet fetched is visible to FAST_FORWARD / default cursors.
SET NOCOUNT ON;
CREATE TABLE dbo.T (Id int NOT NULL PRIMARY KEY, Done bit NOT NULL, Grp int NOT NULL);
INSERT dbo.T VALUES (1,0,1),(2,0,1),(3,0,2),(4,0,2),(5,0,3);
DECLARE @id int, @seen nvarchar(100) = N'';
DECLARE c CURSOR FAST_FORWARD READ_ONLY FOR SELECT Id FROM dbo.T WHERE Done = 0 ORDER BY Id;
OPEN c; FETCH NEXT FROM c INTO @id;
WHILE @@FETCH_STATUS = 0
BEGIN
  SET @seen += CAST(@id AS nvarchar(5)) + N',';
  UPDATE dbo.T SET Done = 1 WHERE Id = @id;      -- keyed, current row only
  FETCH NEXT FROM c INTO @id;
END; CLOSE c; DEALLOCATE c;
SELECT 'keyed' AS probe, @seen AS visited;       -- expect 1,2,3,4,5,
UPDATE dbo.T SET Done = 0;
SET @seen = N'';
DECLARE c2 CURSOR FAST_FORWARD READ_ONLY FOR SELECT Id FROM dbo.T WHERE Done = 0 ORDER BY Id;
OPEN c2; FETCH NEXT FROM c2 INTO @id;
WHILE @@FETCH_STATUS = 0
BEGIN
  SET @seen += CAST(@id AS nvarchar(5)) + N',';
  UPDATE dbo.T SET Done = 1 WHERE Grp = (SELECT Grp FROM dbo.T WHERE Id = @id);  -- reaches unfetched rows
  FETCH NEXT FROM c2 INTO @id;
END; CLOSE c2; DEALLOCATE c2;
SELECT 'group' AS probe, @seen AS visited;       -- snapshot would give 1,2,3,4,5,
DROP TABLE dbo.T;
