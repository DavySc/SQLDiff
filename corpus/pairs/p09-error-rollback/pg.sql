CREATE PROCEDURE add_order_line(p_order integer, p_line integer, p_item integer, p_qty integer)
LANGUAGE plpgsql
AS $$
BEGIN
  INSERT INTO order_line (order_id, line_no, item_id, qty, unit_price)
  SELECT p_order, p_line, p_item, p_qty, p.unit_price
    FROM item_price p
   WHERE p.item_id = p_item;
  UPDATE stock SET qty = qty - p_qty WHERE item_id = p_item;
  COMMIT;
END;
$$;
