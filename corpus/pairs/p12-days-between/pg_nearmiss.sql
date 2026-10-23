CREATE VIEW order_ship_days AS
SELECT order_id,
       EXTRACT(DAY FROM shipped_at - created_at)::integer AS ship_days
  FROM orders;
