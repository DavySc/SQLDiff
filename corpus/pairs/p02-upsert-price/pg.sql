CREATE PROCEDURE set_item_price(p_item integer, p_price numeric(12,2))
LANGUAGE sql
AS $$
  INSERT INTO item_price (item_id, unit_price, updated_at)
  VALUES (p_item, p_price, statement_timestamp())
  ON CONFLICT (item_id) DO UPDATE
     SET unit_price = EXCLUDED.unit_price,
         updated_at = EXCLUDED.updated_at;
$$;
