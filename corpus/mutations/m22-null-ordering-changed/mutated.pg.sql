CREATE FUNCTION oldest_shipments(p_customer integer)
RETURNS TABLE (order_id integer, shipped_at timestamp)
LANGUAGE sql STABLE
AS $$
  SELECT o.order_id, o.shipped_at
    FROM orders o
   WHERE o.customer_id = p_customer
   ORDER BY o.shipped_at, o.order_id
   LIMIT 10;
$$;
