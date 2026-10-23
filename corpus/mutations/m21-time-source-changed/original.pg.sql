CREATE PROCEDURE ship_orders(p_customer integer)
LANGUAGE plpgsql
AS $$
BEGIN
  UPDATE orders
     SET status = 'S', shipped_at = now()
   WHERE customer_id = p_customer
     AND status = 'N';
END;
$$;
