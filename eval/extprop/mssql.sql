-- Probe for SQL Server 2022 (not run in the sandbox: no SQL Server there).
-- Question: is a bracketed identifier in an EXEC argument the unbracketed
-- name string? The converter and engine read `[Person]` as 'Person' for
-- @level0name/@level1name/@level2name of sp_addextendedproperty
-- (AdventureWorks' own install script writes it this way).
-- Expected: three rows whose objname is 'Address' / 'AddressID' and whose
-- value matches; the brackets are NOT part of any stored name.
CREATE SCHEMA [Person Probe];
GO
CREATE TABLE [Person Probe].[Address Lines]([Address ID] int NOT NULL);
GO
EXECUTE [sys].[sp_addextendedproperty] N'MS_Description', N'col', N'SCHEMA', [Person Probe], N'TABLE', [Address Lines], N'COLUMN', [Address ID];
EXEC sys.sp_addextendedproperty @name = N'MS_Description', @value = N'tbl', @level0type = N'SCHEMA', @level0name = [Person Probe], @level1type = N'TABLE', @level1name = [Address Lines];
GO
SELECT objname, name, value FROM fn_listextendedproperty(N'MS_Description', N'SCHEMA', N'Person Probe', N'TABLE', N'Address Lines', N'COLUMN', NULL);
SELECT objname, name, value FROM fn_listextendedproperty(N'MS_Description', N'SCHEMA', N'Person Probe', N'TABLE', NULL, NULL, NULL);
GO
DROP TABLE [Person Probe].[Address Lines];
DROP SCHEMA [Person Probe];
