CREATE PROCEDURE dbo.purge_bad_feed AS
BEGIN
  SET NOCOUNT ON;
  SET XACT_ABORT ON;
  DECLARE @rc int;
  BEGIN TRAN;
  EXEC @rc = sp_getapplock @Resource = 'price-feed', @LockMode = 'Exclusive',
                           @LockOwner = 'Transaction', @LockTimeout = -1;
  IF @rc < 0
  BEGIN
    ROLLBACK;
    THROW 50020, 'lock not granted', 1;
  END
  DELETE FROM dbo.price_feed WHERE unit_price <= 0;
  COMMIT;
END
GO
