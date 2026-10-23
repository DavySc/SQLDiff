CREATE PROCEDURE transfer_stock (IN p_from INTEGER, IN p_to INTEGER, IN p_qty INTEGER)
LANGUAGE SQL
MODIFIES SQL DATA
SET OPTION COMMIT = *CHG
BEGIN ATOMIC
  UPDATE stock SET qty = qty - p_qty WHERE item_id = p_from;
  UPDATE stock SET qty = qty + p_qty WHERE item_id = p_to;
  INSERT INTO stock_log (item_id, delta, reason) VALUES (p_from, -p_qty, 'transfer');
  INSERT INTO stock_log (item_id, delta, reason) VALUES (p_to, p_qty, 'transfer');
END;
