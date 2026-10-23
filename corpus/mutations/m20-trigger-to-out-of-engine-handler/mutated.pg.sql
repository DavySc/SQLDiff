-- Invoked by the application for each updated item after the update commits.
CREATE PROCEDURE stock_audit_handler(p_item integer)
LANGUAGE plpgsql
AS $$
BEGIN
  INSERT INTO audit_event (table_name, key_value, action, event_at)
  VALUES ('stock', p_item::varchar, 'U', statement_timestamp());
END;
$$;
