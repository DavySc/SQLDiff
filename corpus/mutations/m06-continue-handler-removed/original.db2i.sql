CREATE PROCEDURE import_customer (IN p_id INTEGER, IN p_email VARCHAR(254), IN p_last VARCHAR(50),
                                  OUT p_skipped INTEGER)
LANGUAGE SQL
MODIFIES SQL DATA
BEGIN
  DECLARE CONTINUE HANDLER FOR SQLSTATE '23505' SET p_skipped = 1;
  SET p_skipped = 0;
  INSERT INTO customer (customer_id, email, last_name, created_at)
  VALUES (p_id, p_email, p_last, CURRENT TIMESTAMP);
  INSERT INTO audit_event (table_name, key_value, action, event_at)
  VALUES ('customer', VARCHAR(p_id), 'I', CURRENT TIMESTAMP);
END;
