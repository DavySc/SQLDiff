CREATE PROCEDURE set_item_price (IN p_item INTEGER, IN p_price DECIMAL(12,2))
LANGUAGE SQL
MODIFIES SQL DATA
BEGIN
  MERGE INTO item_price t
  USING (VALUES (p_item, p_price)) AS s (item_id, unit_price)
     ON t.item_id = s.item_id
  WHEN MATCHED THEN
    UPDATE SET unit_price = s.unit_price, updated_at = CURRENT TIMESTAMP
  WHEN NOT MATCHED THEN
    INSERT (item_id, unit_price, updated_at) VALUES (s.item_id, s.unit_price, CURRENT TIMESTAMP);
END;
