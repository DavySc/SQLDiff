CREATE OR REPLACE FUNCTION move_stock(p_item int, p_qty int) RETURNS void
LANGUAGE plpgsql
AS $$
BEGIN
  UPDATE stock SET qty = qty - p_qty WHERE item_id = p_item;
  INSERT INTO stock_log (item_id, delta) VALUES (p_item, -p_qty);
END;
$$;
