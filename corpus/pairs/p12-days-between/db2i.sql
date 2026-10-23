CREATE VIEW order_ship_days AS
SELECT order_id,
       DAYS(shipped_at) - DAYS(created_at) AS ship_days
  FROM orders;
