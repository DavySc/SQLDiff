-- Probe for SQL Server 2022 (not run in the sandbox: no SQL Server there).
-- Question: does SQL Server resolve a bracketed identifier in an EXEC
-- argument to the object with the unbracketed name? The converter and engine
-- read `[Person]` as 'Person' for @level0name/@level1name/@level2name of
-- sp_addextendedproperty (AdventureWorks' own install script writes it so).
-- What decides it: the EXECs below succeed (a wrong name raises error 15135)
-- and the properties show on the table and column in SSMS. The SELECTs only
-- list the properties by their catalog names (sys.extended_properties stores
-- object ids, not the typed text), so they cannot show whether brackets were
-- kept; they confirm the properties landed on the intended objects.
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
