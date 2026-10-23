CREATE PROCEDURE dbo.purge_orphan_feed AS
BEGIN
  SET NOCOUNT ON;
  DELETE pf
    FROM dbo.price_feed AS pf
    LEFT JOIN dbo.item AS i ON i.item_id = pf.item_id
   WHERE i.item_id IS NULL;
END
GO
