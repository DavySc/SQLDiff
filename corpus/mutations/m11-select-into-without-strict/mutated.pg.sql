CREATE FUNCTION customer_by_last_name(p_last varchar) RETURNS integer
LANGUAGE plpgsql STABLE
AS $$
DECLARE
  v_id integer;
BEGIN
  SELECT customer_id INTO v_id FROM customer WHERE last_name = p_last;
  RETURN v_id;
END;
$$;
