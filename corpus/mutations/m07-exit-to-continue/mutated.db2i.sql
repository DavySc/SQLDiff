CREATE PROCEDURE add_order_line_status (IN p_order INTEGER, IN p_line INTEGER, IN p_item INTEGER,
                                        IN p_qty INTEGER, OUT p_status INTEGER)
LANGUAGE SQL
MODIFIES SQL DATA
SET OPTION COMMIT = *CHG
BEGIN
  DECLARE CONTINUE HANDLER FOR SQLEXCEPTION
  BEGIN
    ROLLBACK;
    SET p_status = -1;
  END;
  SET p_status = 0;
  INSERT INTO order_line (order_id, line_no, item_id, qty, unit_price)
  SELECT p_order, p_line, p_item, p_qty, p.unit_price
    FROM item_price p
   WHERE p.item_id = p_item;
  UPDATE stock SET qty = qty - p_qty WHERE item_id = p_item;
  COMMIT;
END;
