CREATE FUNCTION customer_by_email(p_email varchar) RETURNS integer
LANGUAGE sql STABLE
AS $$
  SELECT customer_id FROM customer WHERE email = p_email;
$$;
