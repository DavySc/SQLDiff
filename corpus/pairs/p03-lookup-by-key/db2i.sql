CREATE FUNCTION customer_last_name (p_id INTEGER) RETURNS VARCHAR(50)
LANGUAGE SQL
READS SQL DATA
BEGIN
  DECLARE v_name VARCHAR(50);
  SET v_name = (SELECT last_name FROM customer WHERE customer_id = p_id);
  RETURN v_name;
END;
