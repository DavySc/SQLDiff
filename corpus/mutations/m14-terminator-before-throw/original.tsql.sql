CREATE PROCEDURE dbo.add_order_line @Order int, @Line int, @Item int, @Qty int AS
BEGIN
  SET NOCOUNT ON;
  SET XACT_ABORT ON;
  BEGIN TRY
    BEGIN TRAN;
    INSERT INTO dbo.order_line (order_id, line_no, item_id, qty, unit_price)
    SELECT @Order, @Line, @Item, @Qty, p.unit_price
      FROM dbo.item_price p
     WHERE p.item_id = @Item;
    UPDATE dbo.stock SET qty = qty - @Qty WHERE item_id = @Item;
    COMMIT;
  END TRY
  BEGIN CATCH
    IF @@TRANCOUNT > 0 ROLLBACK;
    THROW;
  END CATCH
END
GO
