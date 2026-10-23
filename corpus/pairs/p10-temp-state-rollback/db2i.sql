CREATE PROCEDURE apply_price_feed ()
LANGUAGE SQL
MODIFIES SQL DATA
SET OPTION COMMIT = *CHG
BEGIN
  DECLARE EXIT HANDLER FOR SQLEXCEPTION
  BEGIN
    ROLLBACK;
    INSERT INTO audit_event (table_name, key_value, action, event_at)
    SELECT 'item_price', VARCHAR(item_id), 'R', CURRENT TIMESTAMP
      FROM SESSION.attempted;
    COMMIT;
  END;
  DECLARE GLOBAL TEMPORARY TABLE SESSION.attempted (item_id INTEGER NOT NULL)
    WITH REPLACE NOT LOGGED ON ROLLBACK PRESERVE ROWS;
  INSERT INTO SESSION.attempted (item_id)
  SELECT p.item_id FROM item_price p
   WHERE EXISTS (SELECT 1 FROM price_feed f WHERE f.item_id = p.item_id);
  UPDATE item_price p
     SET (unit_price, updated_at) =
         (SELECT f.unit_price, CURRENT TIMESTAMP FROM price_feed f WHERE f.item_id = p.item_id)
   WHERE EXISTS (SELECT 1 FROM price_feed f WHERE f.item_id = p.item_id);
  IF EXISTS (SELECT 1 FROM item_price WHERE unit_price <= 0) THEN
    SIGNAL SQLSTATE '75010' SET MESSAGE_TEXT = 'non-positive price';
  END IF;
  COMMIT;
END;
