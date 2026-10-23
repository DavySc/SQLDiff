CREATE PROCEDURE dbo.sync_item_price AS
BEGIN
  SET NOCOUNT ON;
  MERGE dbo.item_price WITH (HOLDLOCK) AS t
  USING dbo.price_feed AS s
     ON t.item_id = s.item_id
  WHEN MATCHED THEN
    UPDATE SET unit_price = s.unit_price, updated_at = SYSDATETIME()
  WHEN NOT MATCHED BY TARGET THEN
    INSERT (item_id, unit_price, updated_at) VALUES (s.item_id, s.unit_price, SYSDATETIME())
  WHEN NOT MATCHED BY SOURCE THEN
    DELETE;
END
GO
