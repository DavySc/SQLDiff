CREATE VIEW order_ship_days AS
SELECT order_id,
       shipped_at::date - created_at::date AS ship_days
  FROM orders;
