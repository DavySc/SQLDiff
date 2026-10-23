CREATE PROCEDURE dbo.oldest_shipments @Customer int AS
BEGIN
  SET NOCOUNT ON;
  SELECT TOP (10) order_id, shipped_at
    FROM dbo.orders
   WHERE customer_id = @Customer
   ORDER BY shipped_at, order_id;
END
GO
