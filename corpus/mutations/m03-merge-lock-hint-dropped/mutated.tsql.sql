CREATE PROCEDURE dbo.set_item_price @Item int, @Price decimal(12,2) AS
BEGIN
  SET NOCOUNT ON;
  MERGE dbo.item_price AS t
  USING (SELECT @Item AS item_id, @Price AS unit_price) AS s
     ON t.item_id = s.item_id
  WHEN MATCHED THEN
    UPDATE SET unit_price = s.unit_price, updated_at = SYSDATETIME()
  WHEN NOT MATCHED THEN
    INSERT (item_id, unit_price, updated_at) VALUES (s.item_id, s.unit_price, SYSDATETIME());
END
GO
