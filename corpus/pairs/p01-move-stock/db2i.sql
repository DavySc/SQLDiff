CREATE PROCEDURE move_stock (IN p_item INTEGER, IN p_qty INTEGER)
LANGUAGE SQL
SET OPTION COMMIT = *NONE
BEGIN
  UPDATE stock SET qty = qty - p_qty WHERE item_id = p_item;
  INSERT INTO stock_log (item_id, delta) VALUES (p_item, -p_qty);
END;
