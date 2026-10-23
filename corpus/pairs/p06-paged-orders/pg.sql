CREATE FUNCTION customer_orders_page(p_customer integer, p_skip integer, p_take integer)
RETURNS TABLE (order_id integer, status char(1), total numeric(12,2), created_at timestamp)
LANGUAGE sql STABLE
AS $$
  SELECT o.order_id, o.status, o.total, o.created_at
    FROM orders o
   WHERE o.customer_id = p_customer
   ORDER BY o.created_at DESC, o.order_id DESC
   LIMIT p_take OFFSET p_skip;
$$;
