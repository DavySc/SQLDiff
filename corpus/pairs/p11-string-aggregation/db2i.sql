CREATE VIEW order_item_list AS
SELECT order_id,
       LISTAGG(VARCHAR(item_id), ',') WITHIN GROUP (ORDER BY line_no) AS item_ids
  FROM order_line
 GROUP BY order_id;
