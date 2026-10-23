CREATE PROCEDURE create_order (IN p_customer INTEGER, OUT p_order_id INTEGER)
LANGUAGE SQL
MODIFIES SQL DATA
BEGIN
  SELECT order_id INTO p_order_id
    FROM FINAL TABLE (
      INSERT INTO orders (customer_id, status, created_at)
      VALUES (p_customer, 'N', CURRENT TIMESTAMP));
END;
