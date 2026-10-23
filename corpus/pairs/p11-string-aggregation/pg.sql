CREATE VIEW order_item_list AS
SELECT order_id,
       string_agg(item_id::text, ',' ORDER BY line_no) AS item_ids
  FROM order_line
 GROUP BY order_id;
