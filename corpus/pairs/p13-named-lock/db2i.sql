CREATE PROCEDURE purge_bad_feed ()
LANGUAGE SQL
MODIFIES SQL DATA
SET OPTION COMMIT = *CHG
BEGIN
  DECLARE v_name VARCHAR(64);
  SELECT lock_name INTO v_name
    FROM app_lock
   WHERE lock_name = 'price-feed'
    WITH RS USE AND KEEP EXCLUSIVE LOCKS;
  DELETE FROM price_feed WHERE unit_price <= 0;
  COMMIT;
END;
