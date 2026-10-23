CREATE FUNCTION dbo.customer_by_email (@Email varchar(254)) RETURNS int AS
BEGIN
  DECLARE @id int;
  SELECT @id = customer_id FROM dbo.customer WHERE email = @Email;
  RETURN @id;
END
GO
