CREATE PROCEDURE add_order_line (IN p_order INTEGER, IN p_line INTEGER, IN p_item INTEGER, IN p_qty INTEGER)
LANGUAGE SQL
MODIFIES SQL DATA
SET OPTION COMMIT = *NONE
BEGIN
  DECLARE EXIT HANDLER FOR SQLEXCEPTION
  BEGIN
    ROLLBACK;
    RESIGNAL;
  END;
  INSERT INTO order_line (order_id, line_no, item_id, qty, unit_price)
  SELECT p_order, p_line, p_item, p_qty, p.unit_price
    FROM item_price p
   WHERE p.item_id = p_item;
  UPDATE stock SET qty = qty - p_qty WHERE item_id = p_item;
  COMMIT;
END;
