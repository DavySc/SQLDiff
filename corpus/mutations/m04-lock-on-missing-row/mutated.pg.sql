CREATE PROCEDURE purge_bad_feed()
LANGUAGE plpgsql
AS $$
BEGIN
  PERFORM 1 FROM app_lock WHERE lock_name = 'price-feed' FOR UPDATE;
  DELETE FROM price_feed WHERE unit_price <= 0;
END;
$$;
