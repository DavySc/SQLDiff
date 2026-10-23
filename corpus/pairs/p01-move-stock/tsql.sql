CREATE PROCEDURE dbo.move_stock @Item int, @Qty int AS
BEGIN
  SET NOCOUNT ON;
  BEGIN TRAN;
  UPDATE dbo.stock SET qty = qty - @Qty WHERE item_id = @Item;
  IF @@ERROR <> 0 BEGIN ROLLBACK; RETURN 1; END
  INSERT INTO dbo.stock_log (item_id, delta) VALUES (@Item, -@Qty);
  IF @@ERROR <> 0 BEGIN ROLLBACK; RETURN 2; END
  COMMIT;
  RETURN 0;
END
GO
