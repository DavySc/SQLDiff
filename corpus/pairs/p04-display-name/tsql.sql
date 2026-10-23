CREATE VIEW dbo.customer_display AS
SELECT customer_id,
       ISNULL(first_name + ' ', '') + last_name AS display_name
  FROM dbo.customer;
GO
