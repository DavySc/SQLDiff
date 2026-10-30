SET NOCOUNT ON;
IF OBJECT_ID('WebApi.UpdateCityFromJson') IS NOT NULL DROP PROCEDURE WebApi.UpdateCityFromJson;
IF OBJECT_ID('Application.Cities') IS NOT NULL DROP TABLE Application.Cities;
IF SCHEMA_ID('Application') IS NULL EXEC('CREATE SCHEMA Application');
IF SCHEMA_ID('WebApi') IS NULL EXEC('CREATE SCHEMA WebApi');
GO
CREATE TABLE Application.Cities (CityID int PRIMARY KEY, CityName nvarchar(50), StateProvinceID int, LatestRecordedPopulation bigint, LastEditedBy int);
INSERT INTO Application.Cities VALUES (1,'A',1,10,0),(2,'B',2,20,0);
GO
CREATE PROCEDURE [WebApi].[UpdateCityFromJson](@City NVARCHAR(MAX), @CityID int,@UserID int)
WITH EXECUTE AS OWNER
AS BEGIN
	UPDATE Application.Cities SET
		CityName = json.CityName,
		StateProvinceID = json.StateProvinceID,
		LatestRecordedPopulation = json.LatestRecordedPopulation,
		LastEditedBy = @UserID
	FROM OPENJSON (@City)
		WITH (
			CityName nvarchar(50) N'strict $.CityName',
			StateProvinceID int N'strict $.StateProvinceID',
			LatestRecordedPopulation bigint) as json
	WHERE
		Application.Cities.CityID = @CityID

END
GO
EXEC WebApi.UpdateCityFromJson N'{"CityName":"Zürich😀","StateProvinceID":7,"LatestRecordedPopulation":null}', 1, 9;
EXEC WebApi.UpdateCityFromJson N'{}', 2, 9;
SELECT * FROM Application.Cities ORDER BY 1;
EXEC WebApi.UpdateCityFromJson N'{"StateProvinceID":7}', 1, 9;
GO
