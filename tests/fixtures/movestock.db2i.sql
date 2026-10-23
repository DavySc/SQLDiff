CREATE PROCEDURE MoveStock (IN p_Item INT, IN p_Qty INT)
LANGUAGE SQL
SET OPTION COMMIT = *NONE
BEGIN
  UPDATE Stock SET Qty = Qty - p_Qty WHERE ItemId = p_Item;
  INSERT INTO StockLog (ItemId, Delta) VALUES (p_Item, -p_Qty);
END
