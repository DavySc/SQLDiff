-- [part:proc-header template=std-proc]
CREATE PROCEDURE dbo.restock @Item int, @Qty int AS
BEGIN
  SET NOCOUNT ON;
-- [/part]
-- [part:body template=restock-body]
  BEGIN TRAN;
  UPDATE dbo.stock SET qty = qty + @Qty WHERE item_id = @Item;
  INSERT INTO dbo.stock_log (item_id, delta, reason) VALUES (@Item, @Qty, 'restock');
  COMMIT;
-- [/part]
-- [part:proc-footer template=std-proc]
END
GO
-- [/part]
