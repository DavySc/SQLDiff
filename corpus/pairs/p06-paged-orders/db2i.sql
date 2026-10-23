CREATE FUNCTION customer_orders_page (p_customer INTEGER, p_skip INTEGER, p_take INTEGER)
RETURNS TABLE (order_id INTEGER, status CHAR(1), total DECIMAL(12,2), created_at TIMESTAMP)
LANGUAGE SQL
READS SQL DATA
RETURN
  SELECT order_id, status, total, created_at
    FROM orders
   WHERE customer_id = p_customer
   ORDER BY created_at DESC, order_id DESC
  OFFSET p_skip ROWS FETCH FIRST p_take ROWS ONLY;
