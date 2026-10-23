CREATE FUNCTION dbo.customer_last_name (@Id int) RETURNS varchar(50) AS
BEGIN
  DECLARE @name varchar(50);
  SELECT @name = last_name FROM dbo.customer WHERE customer_id = @Id;
  RETURN @name;
END
GO
