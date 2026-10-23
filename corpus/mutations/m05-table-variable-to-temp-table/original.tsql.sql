CREATE PROCEDURE dbo.apply_price_feed AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @attempted TABLE (item_id int NOT NULL);
  BEGIN TRY
    BEGIN TRAN;
    UPDATE p
       SET unit_price = f.unit_price, updated_at = SYSDATETIME()
    OUTPUT inserted.item_id INTO @attempted (item_id)
      FROM dbo.item_price AS p
      JOIN dbo.price_feed AS f ON f.item_id = p.item_id;
    IF EXISTS (SELECT 1 FROM dbo.item_price WHERE unit_price <= 0)
      THROW 50010, 'non-positive price', 1;
    COMMIT;
  END TRY
  BEGIN CATCH
    IF @@TRANCOUNT > 0 ROLLBACK;
    INSERT INTO dbo.audit_event (table_name, key_value, action, event_at)
    SELECT 'item_price', CAST(item_id AS varchar(64)), 'R', SYSDATETIME()
      FROM @attempted;
  END CATCH
END
GO
