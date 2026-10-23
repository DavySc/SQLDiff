CREATE FUNCTION stock_audit() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
  INSERT INTO audit_event (table_name, key_value, action, event_at)
  VALUES ('stock', NEW.item_id::varchar, 'U', statement_timestamp());
  RETURN NEW;
END;
$$;

CREATE TRIGGER stock_audit_trg
AFTER UPDATE ON stock
FOR EACH ROW EXECUTE FUNCTION stock_audit();
