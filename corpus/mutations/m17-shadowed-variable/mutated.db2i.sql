CREATE PROCEDURE recount_order (IN p_order INTEGER, OUT p_total DECIMAL(12,2))
LANGUAGE SQL
READS SQL DATA
BEGIN
  DECLARE v_total DECIMAL(12,2) DEFAULT 0;
  lines: BEGIN
    DECLARE v_total DECIMAL(12,2) DEFAULT 0;
    FOR l AS SELECT qty, unit_price FROM order_line WHERE order_id = p_order DO
      SET v_total = v_total + l.qty * l.unit_price;
    END FOR;
  END lines;
  SET p_total = v_total;
END;
