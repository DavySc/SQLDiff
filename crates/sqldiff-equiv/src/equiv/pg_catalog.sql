-- PostgreSQL 18 system catalogs the evaluator may read, generated from
-- pg_attribute and pg_index of PostgreSQL 18.6. Column nullability is the
-- catalog's own (attnotnull); in the pg_tables and pg_indexes views it
-- follows the view definitions. Types: name -> varchar(63), "char" ->
-- varchar(1); oid-like, array and internal types are left opaque.

CREATE TABLE pg_catalog.pg_attribute (
    attrelid oid NOT NULL,
    attname varchar(63) NOT NULL,
    atttypid oid NOT NULL,
    attlen smallint NOT NULL,
    attnum smallint NOT NULL,
    atttypmod integer NOT NULL,
    attndims smallint NOT NULL,
    attbyval boolean NOT NULL,
    attalign varchar(1) NOT NULL,
    attstorage varchar(1) NOT NULL,
    attcompression varchar(1) NOT NULL,
    attnotnull boolean NOT NULL,
    atthasdef boolean NOT NULL,
    atthasmissing boolean NOT NULL,
    attidentity varchar(1) NOT NULL,
    attgenerated varchar(1) NOT NULL,
    attisdropped boolean NOT NULL,
    attislocal boolean NOT NULL,
    attinhcount smallint NOT NULL,
    attcollation oid NOT NULL,
    attstattarget smallint,
    attacl pg_opaque,
    attoptions pg_opaque,
    attfdwoptions pg_opaque,
    attmissingval pg_opaque,
    UNIQUE (attrelid, attname),
    PRIMARY KEY (attrelid, attnum)
);

CREATE TABLE pg_catalog.pg_class (
    oid oid NOT NULL,
    relname varchar(63) NOT NULL,
    relnamespace oid NOT NULL,
    reltype oid NOT NULL,
    reloftype oid NOT NULL,
    relowner oid NOT NULL,
    relam oid NOT NULL,
    relfilenode oid NOT NULL,
    reltablespace oid NOT NULL,
    relpages integer NOT NULL,
    reltuples real NOT NULL,
    relallvisible integer NOT NULL,
    relallfrozen integer NOT NULL,
    reltoastrelid oid NOT NULL,
    relhasindex boolean NOT NULL,
    relisshared boolean NOT NULL,
    relpersistence varchar(1) NOT NULL,
    relkind varchar(1) NOT NULL,
    relnatts smallint NOT NULL,
    relchecks smallint NOT NULL,
    relhasrules boolean NOT NULL,
    relhastriggers boolean NOT NULL,
    relhassubclass boolean NOT NULL,
    relrowsecurity boolean NOT NULL,
    relforcerowsecurity boolean NOT NULL,
    relispopulated boolean NOT NULL,
    relreplident varchar(1) NOT NULL,
    relispartition boolean NOT NULL,
    relrewrite oid NOT NULL,
    relfrozenxid xid NOT NULL,
    relminmxid xid NOT NULL,
    relacl pg_opaque,
    reloptions pg_opaque,
    relpartbound pg_opaque,
    UNIQUE (relname, relnamespace),
    PRIMARY KEY (oid)
);

CREATE TABLE pg_catalog.pg_constraint (
    oid oid NOT NULL,
    conname varchar(63) NOT NULL,
    connamespace oid NOT NULL,
    contype varchar(1) NOT NULL,
    condeferrable boolean NOT NULL,
    condeferred boolean NOT NULL,
    conenforced boolean NOT NULL,
    convalidated boolean NOT NULL,
    conrelid oid NOT NULL,
    contypid oid NOT NULL,
    conindid oid NOT NULL,
    conparentid oid NOT NULL,
    confrelid oid NOT NULL,
    confupdtype varchar(1) NOT NULL,
    confdeltype varchar(1) NOT NULL,
    confmatchtype varchar(1) NOT NULL,
    conislocal boolean NOT NULL,
    coninhcount smallint NOT NULL,
    connoinherit boolean NOT NULL,
    conperiod boolean NOT NULL,
    conkey pg_opaque,
    confkey pg_opaque,
    conpfeqop pg_opaque,
    conppeqop pg_opaque,
    conffeqop pg_opaque,
    confdelsetcols pg_opaque,
    conexclop pg_opaque,
    conbin pg_opaque,
    UNIQUE (conrelid, contypid, conname),
    PRIMARY KEY (oid)
);

CREATE TABLE pg_catalog.pg_extension (
    oid oid NOT NULL,
    extname varchar(63) NOT NULL,
    extowner oid NOT NULL,
    extnamespace oid NOT NULL,
    extrelocatable boolean NOT NULL,
    extversion text NOT NULL,
    extconfig pg_opaque,
    extcondition pg_opaque,
    UNIQUE (extname),
    PRIMARY KEY (oid)
);

CREATE TABLE pg_catalog.pg_index (
    indexrelid oid NOT NULL,
    indrelid oid NOT NULL,
    indnatts smallint NOT NULL,
    indnkeyatts smallint NOT NULL,
    indisunique boolean NOT NULL,
    indnullsnotdistinct boolean NOT NULL,
    indisprimary boolean NOT NULL,
    indisexclusion boolean NOT NULL,
    indimmediate boolean NOT NULL,
    indisclustered boolean NOT NULL,
    indisvalid boolean NOT NULL,
    indcheckxmin boolean NOT NULL,
    indisready boolean NOT NULL,
    indislive boolean NOT NULL,
    indisreplident boolean NOT NULL,
    indkey pg_opaque NOT NULL,
    indcollation pg_opaque NOT NULL,
    indclass pg_opaque NOT NULL,
    indoption pg_opaque NOT NULL,
    indexprs pg_opaque,
    indpred pg_opaque,
    PRIMARY KEY (indexrelid)
);

CREATE TABLE pg_catalog.pg_indexes (
    schemaname varchar(63) NOT NULL,
    tablename varchar(63) NOT NULL,
    indexname varchar(63) NOT NULL,
    tablespace varchar(63),
    indexdef text NOT NULL
);

CREATE TABLE pg_catalog.pg_inherits (
    inhrelid oid NOT NULL,
    inhparent oid NOT NULL,
    inhseqno integer NOT NULL,
    inhdetachpending boolean NOT NULL,
    PRIMARY KEY (inhrelid, inhseqno)
);

CREATE TABLE pg_catalog.pg_namespace (
    oid oid NOT NULL,
    nspname varchar(63) NOT NULL,
    nspowner oid NOT NULL,
    nspacl pg_opaque,
    UNIQUE (nspname),
    PRIMARY KEY (oid)
);

CREATE TABLE pg_catalog.pg_partitioned_table (
    partrelid oid NOT NULL,
    partstrat varchar(1) NOT NULL,
    partnatts smallint NOT NULL,
    partdefid oid NOT NULL,
    partattrs pg_opaque NOT NULL,
    partclass pg_opaque NOT NULL,
    partcollation pg_opaque NOT NULL,
    partexprs pg_opaque,
    PRIMARY KEY (partrelid)
);

CREATE TABLE pg_catalog.pg_publication (
    oid oid NOT NULL,
    pubname varchar(63) NOT NULL,
    pubowner oid NOT NULL,
    puballtables boolean NOT NULL,
    pubinsert boolean NOT NULL,
    pubupdate boolean NOT NULL,
    pubdelete boolean NOT NULL,
    pubtruncate boolean NOT NULL,
    pubviaroot boolean NOT NULL,
    pubgencols varchar(1) NOT NULL,
    PRIMARY KEY (oid),
    UNIQUE (pubname)
);

CREATE TABLE pg_catalog.pg_publication_rel (
    oid oid NOT NULL,
    prpubid oid NOT NULL,
    prrelid oid NOT NULL,
    prqual pg_opaque,
    prattrs pg_opaque,
    UNIQUE (prrelid, prpubid),
    PRIMARY KEY (oid)
);

CREATE TABLE pg_catalog.pg_tables (
    schemaname varchar(63) NOT NULL,
    tablename varchar(63) NOT NULL,
    tableowner varchar(63) NOT NULL,
    tablespace varchar(63),
    hasindexes boolean NOT NULL,
    hasrules boolean NOT NULL,
    hastriggers boolean NOT NULL,
    rowsecurity boolean NOT NULL
);

CREATE TABLE pg_catalog.pg_tablespace (
    oid oid NOT NULL,
    spcname varchar(63) NOT NULL,
    spcowner oid NOT NULL,
    spcacl pg_opaque,
    spcoptions pg_opaque,
    PRIMARY KEY (oid),
    UNIQUE (spcname)
);

CREATE TABLE pg_catalog.pg_type (
    oid oid NOT NULL,
    typname varchar(63) NOT NULL,
    typnamespace oid NOT NULL,
    typowner oid NOT NULL,
    typlen smallint NOT NULL,
    typbyval boolean NOT NULL,
    typtype varchar(1) NOT NULL,
    typcategory varchar(1) NOT NULL,
    typispreferred boolean NOT NULL,
    typisdefined boolean NOT NULL,
    typdelim varchar(1) NOT NULL,
    typrelid oid NOT NULL,
    typsubscript oid NOT NULL,
    typelem oid NOT NULL,
    typarray oid NOT NULL,
    typinput oid NOT NULL,
    typoutput oid NOT NULL,
    typreceive oid NOT NULL,
    typsend oid NOT NULL,
    typmodin oid NOT NULL,
    typmodout oid NOT NULL,
    typanalyze oid NOT NULL,
    typalign varchar(1) NOT NULL,
    typstorage varchar(1) NOT NULL,
    typnotnull boolean NOT NULL,
    typbasetype oid NOT NULL,
    typtypmod integer NOT NULL,
    typndims integer NOT NULL,
    typcollation oid NOT NULL,
    typdefaultbin pg_opaque,
    typdefault text,
    typacl pg_opaque,
    UNIQUE (typname, typnamespace),
    PRIMARY KEY (oid)
);
