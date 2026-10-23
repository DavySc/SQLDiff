CREATE PROCEDURE refresh_prices ()
LANGUAGE SQL
MODIFIES SQL DATA
BEGIN
  UPDATE item_price p
     SET unit_price = (SELECT f.unit_price FROM price_feed f WHERE f.item_id = p.item_id);
END;
