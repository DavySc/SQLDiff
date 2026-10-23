CREATE FUNCTION dbo.customer_orders_page (@Customer int, @Skip int, @Take int)
RETURNS TABLE AS
RETURN
  SELECT order_id, status, total, created_at
    FROM dbo.orders
   WHERE customer_id = @Customer
   ORDER BY created_at DESC, order_id DESC
  OFFSET @Skip ROWS FETCH NEXT @Take ROWS ONLY;
GO
