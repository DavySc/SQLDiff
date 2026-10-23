CREATE PROCEDURE dbo.create_order @Customer int, @OrderId int OUTPUT AS
BEGIN
  SET NOCOUNT ON;
  INSERT INTO dbo.orders (customer_id, status, created_at)
  VALUES (@Customer, 'N', SYSDATETIME());
  SET @OrderId = SCOPE_IDENTITY();
END
GO
