-- Same procedure, reformatted: different case, layout, comments and quoting.
create procedure [dbo].[MoveStock]
    @Item int,
    @Qty  int
as
begin
    set nocount on;
    begin tran;

    update Stock
       set Qty = Qty - @Qty
     where ItemId = @Item;   -- decrement
    if @@ERROR <> 0
    begin
        rollback;
        return 1;
    end

    insert into StockLog (ItemId, Delta)
    values (@Item, -@Qty);
    if @@ERROR <> 0 begin rollback; return 2; end

    commit;
    return 0;
end
