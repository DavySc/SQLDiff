CREATE VIEW dbo.order_item_list AS
SELECT order_id,
       STRING_AGG(CAST(item_id AS varchar(11)), ',') WITHIN GROUP (ORDER BY line_no) AS item_ids
  FROM dbo.order_line
 GROUP BY order_id;
GO
