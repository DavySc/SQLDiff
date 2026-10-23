-- Synthetic catalog shared by every corpus case (PostgreSQL).
-- The same schema exists in tsql.sql and db2i.sql; keep the three in step.

CREATE TABLE customer (
    customer_id integer      PRIMARY KEY,
    email       varchar(254) NOT NULL,
    first_name  varchar(50),
    last_name   varchar(50)  NOT NULL,
    status      char(1)      NOT NULL DEFAULT 'A',
    created_at  timestamp    NOT NULL
);
CREATE UNIQUE INDEX customer_email_uq ON customer (email);

CREATE TABLE item (
    item_id  integer      PRIMARY KEY,
    name     varchar(100) NOT NULL,
    category varchar(20)
);

CREATE TABLE stock (
    item_id integer PRIMARY KEY REFERENCES item (item_id),
    qty     integer NOT NULL
);

CREATE TABLE stock_log (
    log_id    integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    item_id   integer      NOT NULL,
    delta     integer      NOT NULL,
    reason    varchar(40),
    logged_at timestamp    NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE item_price (
    item_id    integer       PRIMARY KEY REFERENCES item (item_id),
    unit_price numeric(12,2) NOT NULL,
    updated_at timestamp
);

-- Staging source for merges; deliberately has no key.
CREATE TABLE price_feed (
    item_id    integer       NOT NULL,
    unit_price numeric(12,2) NOT NULL
);

CREATE TABLE orders (
    order_id    integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    customer_id integer       NOT NULL REFERENCES customer (customer_id),
    status      char(1)       NOT NULL,
    total       numeric(12,2),
    created_at  timestamp     NOT NULL,
    shipped_at  timestamp
);

CREATE TABLE order_line (
    order_id   integer       NOT NULL REFERENCES orders (order_id),
    line_no    integer       NOT NULL,
    item_id    integer       NOT NULL REFERENCES item (item_id),
    qty        integer       NOT NULL,
    unit_price numeric(12,2) NOT NULL,
    PRIMARY KEY (order_id, line_no)
);

-- Rows used to emulate named application locks.
CREATE TABLE app_lock (
    lock_name varchar(64) PRIMARY KEY
);

CREATE TABLE audit_event (
    event_id   integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    table_name varchar(64) NOT NULL,
    key_value  varchar(64) NOT NULL,
    action     char(1)     NOT NULL,
    event_at   timestamp   NOT NULL
);

-- Routine whose signature the binding checks rely on.
CREATE PROCEDURE log_stock_change(p_item integer, p_delta integer, p_reason varchar)
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO stock_log (item_id, delta, reason) VALUES (p_item, p_delta, p_reason);
END;
$$;
