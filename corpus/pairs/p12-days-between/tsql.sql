CREATE VIEW dbo.order_ship_days AS
SELECT order_id,
       DATEDIFF(day, created_at, shipped_at) AS ship_days
  FROM dbo.orders;
GO
