CREATE FUNCTION move_stock(p_item integer, p_qty integer) RETURNS integer
LANGUAGE plpgsql
AS $$
BEGIN
  UPDATE stock SET qty = qty - p_qty WHERE item_id = p_item;
  INSERT INTO stock_log (item_id, delta) VALUES (p_item, -p_qty);
  RETURN 0;
END;
$$;
