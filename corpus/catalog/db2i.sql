-- Synthetic catalog shared by every corpus case (Db2 for i, SQL naming).
-- The same schema exists in tsql.sql and pg.sql; keep the three in step.
-- Objects are unqualified and resolved through the default schema or library list.
-- Tables are assumed to be journaled, so commitment control is possible.

CREATE TABLE customer (
    customer_id INTEGER      NOT NULL PRIMARY KEY,
    email       VARCHAR(254) NOT NULL,
    first_name  VARCHAR(50),
    last_name   VARCHAR(50)  NOT NULL,
    status      CHAR(1)      NOT NULL DEFAULT 'A',
    created_at  TIMESTAMP    NOT NULL
);
CREATE UNIQUE INDEX customer_email_uq ON customer (email);

CREATE TABLE item (
    item_id  INTEGER      NOT NULL PRIMARY KEY,
    name     VARCHAR(100) NOT NULL,
    category VARCHAR(20)
);

CREATE TABLE stock (
    item_id INTEGER NOT NULL PRIMARY KEY REFERENCES item (item_id),
    qty     INTEGER NOT NULL
);

CREATE TABLE stock_log (
    log_id    INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    item_id   INTEGER     NOT NULL,
    delta     INTEGER     NOT NULL,
    reason    VARCHAR(40),
    logged_at TIMESTAMP   NOT NULL DEFAULT CURRENT TIMESTAMP
);

CREATE TABLE item_price (
    item_id    INTEGER       NOT NULL PRIMARY KEY REFERENCES item (item_id),
    unit_price DECIMAL(12,2) NOT NULL,
    updated_at TIMESTAMP
);

-- Staging source for merges; deliberately has no key.
CREATE TABLE price_feed (
    item_id    INTEGER       NOT NULL,
    unit_price DECIMAL(12,2) NOT NULL
);

CREATE TABLE orders (
    order_id    INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    customer_id INTEGER       NOT NULL REFERENCES customer (customer_id),
    status      CHAR(1)       NOT NULL,
    total       DECIMAL(12,2),
    created_at  TIMESTAMP     NOT NULL,
    shipped_at  TIMESTAMP
);

CREATE TABLE order_line (
    order_id   INTEGER       NOT NULL REFERENCES orders (order_id),
    line_no    INTEGER       NOT NULL,
    item_id    INTEGER       NOT NULL REFERENCES item (item_id),
    qty        INTEGER       NOT NULL,
    unit_price DECIMAL(12,2) NOT NULL,
    PRIMARY KEY (order_id, line_no)
);

-- Rows used to emulate named application locks.
CREATE TABLE app_lock (
    lock_name VARCHAR(64) NOT NULL PRIMARY KEY
);

CREATE TABLE audit_event (
    event_id   INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    table_name VARCHAR(64) NOT NULL,
    key_value  VARCHAR(64) NOT NULL,
    action     CHAR(1)     NOT NULL,
    event_at   TIMESTAMP   NOT NULL
);

-- Routine whose signature the binding checks rely on.
CREATE PROCEDURE log_stock_change (IN p_item INTEGER, IN p_delta INTEGER, IN p_reason VARCHAR(40))
LANGUAGE SQL
MODIFIES SQL DATA
BEGIN
    INSERT INTO stock_log (item_id, delta, reason) VALUES (p_item, p_delta, p_reason);
END;
