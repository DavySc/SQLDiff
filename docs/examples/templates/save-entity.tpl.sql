<%@include header.sql %>
-- A generator template: <%NAME%> placeholders follow docs/examples/pack.
CREATE PROCEDURE <%SCHEMA%>.Save<%ENTITY%>
    @Id int,
    @Name nvarchar(100),
    @Price decimal(12,2)
AS
BEGIN
  SET NOCOUNT ON;
  SET XACT_ABORT ON;
  IF @Price < 0
    THROW 50001, 'Negative price', 1;
  BEGIN TRY
    BEGIN TRAN;
    UPDATE <%SCHEMA%>.<%TABLE%>
       SET [Name] = @Name, Price = @Price, UpdatedAt = SYSUTCDATETIME()
     WHERE Id = @Id;
    INSERT INTO <%SCHEMA%>.<%TABLE%>_log (Id, Note) VALUES (@Id, 'saved ' + @Name);
    COMMIT;
  END TRY
  BEGIN CATCH
    IF @@TRANCOUNT > 0 ROLLBACK;
    THROW;
  END CATCH
END
