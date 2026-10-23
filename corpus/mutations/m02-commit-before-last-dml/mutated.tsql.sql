CREATE PROCEDURE dbo.adjust_stock @Item int, @Delta int AS
BEGIN
  SET NOCOUNT ON;
  SET XACT_ABORT ON;
  BEGIN TRAN;
  UPDATE dbo.stock SET qty = qty + @Delta WHERE item_id = @Item;
  COMMIT;
  INSERT INTO dbo.stock_log (item_id, delta, reason) VALUES (@Item, @Delta, 'adjust');
END
GO
