CREATE PROCEDURE dbo.write_off_stock @Item int, @Qty int AS
BEGIN
  SET NOCOUNT ON;
  UPDATE dbo.stock SET qty = qty - @Qty WHERE item_id = @Item;
  DECLARE @delta int = -@Qty;
  EXEC dbo.log_stock_change @Item = @Item, @Delta = @delta, @Reason = 'write-off';
END
GO
