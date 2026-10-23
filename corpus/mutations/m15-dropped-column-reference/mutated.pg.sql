CREATE FUNCTION available_qty(p_item integer) RETURNS integer
LANGUAGE plpgsql STABLE
AS $$
DECLARE
  v_qty integer;
BEGIN
  SELECT qty - reserved_qty INTO v_qty FROM stock WHERE item_id = p_item;
  RETURN v_qty;
END;
$$;
