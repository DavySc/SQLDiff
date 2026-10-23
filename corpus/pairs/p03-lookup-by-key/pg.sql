CREATE FUNCTION customer_last_name(p_id integer) RETURNS varchar(50)
LANGUAGE plpgsql STABLE
AS $$
DECLARE
  v_name varchar(50);
BEGIN
  SELECT last_name INTO v_name FROM customer WHERE customer_id = p_id;
  RETURN v_name;
END;
$$;
