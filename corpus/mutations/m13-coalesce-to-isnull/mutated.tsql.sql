CREATE VIEW dbo.customer_greeting AS
SELECT customer_id,
       ISNULL(first_name, email) AS greeting_name
  FROM dbo.customer;
GO
