DROP SCHEMA IF EXISTS application CASCADE; DROP SCHEMA IF EXISTS webapi CASCADE;
CREATE SCHEMA Application; CREATE SCHEMA WebApi;
CREATE TABLE Application.Cities (CityID integer PRIMARY KEY, CityName varchar(50), StateProvinceID integer, LatestRecordedPopulation bigint, LastEditedBy integer);
INSERT INTO Application.Cities VALUES (1,'A',1,10,0),(2,'B',2,20,0);
CREATE OR REPLACE PROCEDURE WebApi.UpdateCityFromJson(p_city text, p_city_id integer, p_user_id integer)
LANGUAGE plpgsql
SECURITY DEFINER
AS $$
BEGIN
  UPDATE Application.Cities SET CityName = json.CityName,
  		StateProvinceID = json.StateProvinceID,
  		LatestRecordedPopulation = json.LatestRecordedPopulation,
  		LastEditedBy = p_user_id FROM (SELECT CAST(jsonb_path_query_first(e, 'strict $."CityName"') #>> '{}' AS varchar(50)) AS CityName, CAST(jsonb_path_query_first(e, 'strict $."StateProvinceID"') #>> '{}' AS integer) AS StateProvinceID, CAST(e ->> 'LatestRecordedPopulation' AS bigint) AS LatestRecordedPopulation FROM jsonb_array_elements(CASE jsonb_typeof(CAST(p_city AS jsonb)) WHEN 'object' THEN CASE CAST(p_city AS jsonb) WHEN '{}' THEN '[]' ELSE jsonb_build_array(CAST(p_city AS jsonb)) END ELSE CAST(p_city AS jsonb) END) AS e) as json WHERE Application.Cities.CityID = p_city_id;
END;
$$;
CALL WebApi.UpdateCityFromJson('{"CityName":"Zürich😀","StateProvinceID":7,"LatestRecordedPopulation":null}', 1, 9);
CALL WebApi.UpdateCityFromJson('{}', 2, 9);
SELECT * FROM Application.Cities ORDER BY 1;
CALL WebApi.UpdateCityFromJson('{"StateProvinceID":7}', 1, 9);
