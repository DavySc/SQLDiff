CREATE PROCEDURE purge_bad_feed()
LANGUAGE plpgsql
AS $$
BEGIN
  PERFORM pg_advisory_xact_lock(hashtext('price-feed'));
  DELETE FROM price_feed WHERE unit_price <= 0;
END;
$$;
