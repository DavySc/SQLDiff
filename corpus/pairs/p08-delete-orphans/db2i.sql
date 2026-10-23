CREATE PROCEDURE purge_orphan_feed ()
LANGUAGE SQL
MODIFIES SQL DATA
BEGIN
  DELETE FROM price_feed pf
   WHERE NOT EXISTS (SELECT 1 FROM item i WHERE i.item_id = pf.item_id);
END;
