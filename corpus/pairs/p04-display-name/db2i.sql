CREATE VIEW customer_display AS
SELECT customer_id,
       COALESCE(first_name || ' ', '') || last_name AS display_name
  FROM customer;
