CREATE PROCEDURE create_order(p_customer integer, INOUT p_order_id integer DEFAULT NULL)
LANGUAGE plpgsql
AS $$
BEGIN
  INSERT INTO orders (customer_id, status, created_at)
  VALUES (p_customer, 'N', statement_timestamp())
  RETURNING order_id INTO p_order_id;
END;
$$;
