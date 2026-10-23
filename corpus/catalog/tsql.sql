-- Synthetic catalog shared by every corpus case (T-SQL).
-- The same schema exists in pg.sql and db2i.sql; keep the three in step.
-- Assumed database collation: case-insensitive (for example Latin1_General_CI_AS).

CREATE TABLE dbo.customer (
    customer_id int          NOT NULL PRIMARY KEY,
    email       varchar(254) NOT NULL,
    first_name  varchar(50)  NULL,
    last_name   varchar(50)  NOT NULL,
    status      char(1)      NOT NULL DEFAULT 'A',
    created_at  datetime2    NOT NULL
);
CREATE UNIQUE INDEX customer_email_uq ON dbo.customer (email);

CREATE TABLE dbo.item (
    item_id  int          NOT NULL PRIMARY KEY,
    name     varchar(100) NOT NULL,
    category varchar(20)  NULL
);

CREATE TABLE dbo.stock (
    item_id int NOT NULL PRIMARY KEY REFERENCES dbo.item (item_id),
    qty     int NOT NULL
);

CREATE TABLE dbo.stock_log (
    log_id    int IDENTITY(1,1) NOT NULL PRIMARY KEY,
    item_id   int         NOT NULL,
    delta     int         NOT NULL,
    reason    varchar(40) NULL,
    logged_at datetime2   NOT NULL DEFAULT SYSDATETIME()
);

CREATE TABLE dbo.item_price (
    item_id    int           NOT NULL PRIMARY KEY REFERENCES dbo.item (item_id),
    unit_price decimal(12,2) NOT NULL,
    updated_at datetime2     NULL
);

-- Staging source for merges; deliberately has no key.
CREATE TABLE dbo.price_feed (
    item_id    int           NOT NULL,
    unit_price decimal(12,2) NOT NULL
);

CREATE TABLE dbo.orders (
    order_id    int IDENTITY(1,1) NOT NULL PRIMARY KEY,
    customer_id int           NOT NULL REFERENCES dbo.customer (customer_id),
    status      char(1)       NOT NULL,
    total       decimal(12,2) NULL,
    created_at  datetime2     NOT NULL,
    shipped_at  datetime2     NULL
);

CREATE TABLE dbo.order_line (
    order_id   int           NOT NULL REFERENCES dbo.orders (order_id),
    line_no    int           NOT NULL,
    item_id    int           NOT NULL REFERENCES dbo.item (item_id),
    qty        int           NOT NULL,
    unit_price decimal(12,2) NOT NULL,
    PRIMARY KEY (order_id, line_no)
);

-- Rows used to emulate named application locks.
CREATE TABLE dbo.app_lock (
    lock_name varchar(64) NOT NULL PRIMARY KEY
);

CREATE TABLE dbo.audit_event (
    event_id   int IDENTITY(1,1) NOT NULL PRIMARY KEY,
    table_name varchar(64) NOT NULL,
    key_value  varchar(64) NOT NULL,
    action     char(1)     NOT NULL,
    event_at   datetime2   NOT NULL
);
GO

-- Routine whose signature the binding checks rely on.
CREATE PROCEDURE dbo.log_stock_change @Item int, @Delta int, @Reason varchar(40) AS
BEGIN
    SET NOCOUNT ON;
    INSERT INTO dbo.stock_log (item_id, delta, reason) VALUES (@Item, @Delta, @Reason);
END
GO
