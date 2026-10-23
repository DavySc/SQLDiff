CREATE PROCEDURE apply_price_feed()
LANGUAGE plpgsql
AS $$
BEGIN
  CREATE TEMP TABLE IF NOT EXISTS attempted (item_id integer NOT NULL) ON COMMIT DROP;
  BEGIN
    WITH upd AS (
      UPDATE item_price p
         SET unit_price = f.unit_price, updated_at = statement_timestamp()
        FROM price_feed f
       WHERE f.item_id = p.item_id
      RETURNING p.item_id
    )
    INSERT INTO attempted (item_id) SELECT item_id FROM upd;
    IF EXISTS (SELECT 1 FROM item_price WHERE unit_price <= 0) THEN
      RAISE EXCEPTION 'non-positive price';
    END IF;
  EXCEPTION WHEN OTHERS THEN
    INSERT INTO audit_event (table_name, key_value, action, event_at)
    SELECT 'item_price', item_id::varchar, 'R', statement_timestamp()
      FROM attempted;
  END;
END;
$$;
