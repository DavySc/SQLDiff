CREATE FUNCTION dbo.customer_by_last_name (@Last varchar(50)) RETURNS int AS
BEGIN
  DECLARE @id int;
  SET @id = (SELECT customer_id FROM dbo.customer WHERE last_name = @Last);
  RETURN @id;
END
GO
