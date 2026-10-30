SET NOCOUNT ON;
CREATE TABLE #i ([InvoiceID] INT NOT NULL, [ReturnedDeliveryData] NVARCHAR (MAX) NULL,
    [ConfirmedDeliveryTime] AS (TRY_CONVERT([datetime2](7),json_value([ReturnedDeliveryData],N'$.DeliveredWhen'),(126))),
    [ConfirmedReceivedBy] AS (json_value([ReturnedDeliveryData],N'$.ReceivedBy')),
    CHECK ([ReturnedDeliveryData] IS NULL OR isjson([ReturnedDeliveryData])<>(0)));
INSERT INTO #i (InvoiceID, ReturnedDeliveryData) VALUES (1, N'{"Events":[],"DeliveredWhen":"2013-01-01T12:34:56.","ReceivedBy":"Aakriti Byrraju"}'), (2, N'{"DeliveredWhen":"2013-01-01T12:34:56","ReceivedBy":{"a":1}}'), (3, NULL), (4, N'{"DeliveredWhen":"2016-02-29T23:59:59.123456"}'), (5, N'{"DeliveredWhen":"2013-02-30T00:00:00"}'), (6, N'{"DeliveredWhen":{"x":"2013-01-01T00:00:00"}}');
SELECT CONCAT(InvoiceID, '|', ISNULL(CONVERT(varchar(26), ConfirmedDeliveryTime, 121), 'NULL'), '|', ISNULL(ConfirmedReceivedBy, N'NULL')) FROM #i ORDER BY InvoiceID;
BEGIN TRY INSERT INTO #i (InvoiceID, ReturnedDeliveryData) VALUES (7, N'5'); SELECT '7|inserted'; END TRY BEGIN CATCH SELECT CONCAT('7|ERR ', ERROR_NUMBER()); END CATCH;
