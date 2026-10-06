-- A small PostgreSQL schema exercising the things sql-er has to handle.

CREATE TABLE shop.customer (
    customer_id serial PRIMARY KEY,
    email       text NOT NULL UNIQUE,
    name        text
);

CREATE TABLE shop.product (
    product_id serial,
    sku        varchar(32) NOT NULL,
    price      numeric(10, 2),
    PRIMARY KEY (product_id)
);

CREATE TABLE shop.orders (
    order_id    serial PRIMARY KEY,
    customer_id int NOT NULL REFERENCES shop.customer,
    placed_at   timestamp with time zone DEFAULT now()
);

CREATE TABLE shop.order_line (
    order_id   int NOT NULL,
    line_no    int NOT NULL,
    product_id int,
    qty        int NOT NULL DEFAULT 1,
    PRIMARY KEY (order_id, line_no)
);

-- Foreign keys added after the fact, as pg_dump does.
ALTER TABLE ONLY shop.order_line
    ADD CONSTRAINT order_line_order_fk FOREIGN KEY (order_id) REFERENCES shop.orders (order_id);
ALTER TABLE ONLY shop.order_line
    ADD CONSTRAINT order_line_product_fk FOREIGN KEY (product_id) REFERENCES shop.product (product_id);

-- References a table that isn't defined here.
CREATE TABLE shop.audit (
    audit_id serial PRIMARY KEY,
    user_id  int REFERENCES auth.users (id)
);

-- A table with no relationships.
CREATE TABLE shop.settings (key text PRIMARY KEY, value text);

INSERT INTO shop.settings VALUES ('currency', 'USD');

-- Not valid SQL; should be skipped with a warning.
CREATE FROBNICATOR everything;
