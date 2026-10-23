CREATE PROCEDURE sync_item_price ()
LANGUAGE SQL
MODIFIES SQL DATA
SET OPTION COMMIT = *NONE
BEGIN
  DELETE FROM item_price t
   WHERE NOT EXISTS (SELECT 1 FROM price_feed s WHERE s.item_id = t.item_id);
  MERGE INTO item_price t
  USING price_feed s
     ON t.item_id = s.item_id
  WHEN MATCHED THEN
    UPDATE SET unit_price = s.unit_price, updated_at = CURRENT TIMESTAMP
  WHEN NOT MATCHED THEN
    INSERT (item_id, unit_price, updated_at) VALUES (s.item_id, s.unit_price, CURRENT TIMESTAMP);
END;
