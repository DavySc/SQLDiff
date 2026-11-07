-- SQL Server 2022 catalog views the evaluator may read, generated from
-- sys.all_columns of SQL Server 2022 (16.x); nullability is the view
-- column's is_nullable. Keys are the views' documented identifiers.

CREATE TABLE sys.[all_objects] (
    [name] sysname NOT NULL,
    [object_id] int NOT NULL,
    [principal_id] int,
    [schema_id] int NOT NULL,
    [parent_object_id] int NOT NULL,
    [type] char(2),
    [type_desc] nvarchar(60),
    [create_date] datetime NOT NULL,
    [modify_date] datetime NOT NULL,
    [is_ms_shipped] bit,
    [is_published] bit,
    [is_schema_published] bit,
    UNIQUE ([object_id])
);
GO

CREATE TABLE sys.[check_constraints] (
    [name] sysname NOT NULL,
    [object_id] int NOT NULL,
    [principal_id] int,
    [schema_id] int NOT NULL,
    [parent_object_id] int NOT NULL,
    [type] char(2),
    [type_desc] nvarchar(60),
    [create_date] datetime NOT NULL,
    [modify_date] datetime NOT NULL,
    [is_ms_shipped] bit NOT NULL,
    [is_published] bit NOT NULL,
    [is_schema_published] bit NOT NULL,
    [is_disabled] bit NOT NULL,
    [is_not_for_replication] bit NOT NULL,
    [is_not_trusted] bit NOT NULL,
    [parent_column_id] int NOT NULL,
    [definition] nvarchar(max),
    [uses_database_collation] bit,
    [is_system_named] bit NOT NULL,
    UNIQUE ([object_id])
);
GO

CREATE TABLE sys.[columns] (
    [object_id] int NOT NULL,
    [name] sysname,
    [column_id] int NOT NULL,
    [system_type_id] tinyint NOT NULL,
    [user_type_id] int NOT NULL,
    [max_length] smallint NOT NULL,
    [precision] tinyint NOT NULL,
    [scale] tinyint NOT NULL,
    [collation_name] sysname,
    [is_nullable] bit,
    [is_ansi_padded] bit NOT NULL,
    [is_rowguidcol] bit NOT NULL,
    [is_identity] bit NOT NULL,
    [is_computed] bit,
    [is_filestream] bit NOT NULL,
    [is_replicated] bit,
    [is_non_sql_subscribed] bit,
    [is_merge_published] bit,
    [is_dts_replicated] bit,
    [is_xml_document] bit NOT NULL,
    [xml_collection_id] int NOT NULL,
    [default_object_id] int NOT NULL,
    [rule_object_id] int NOT NULL,
    [is_sparse] bit,
    [is_column_set] bit,
    [generated_always_type] tinyint,
    [generated_always_type_desc] nvarchar(60),
    [encryption_type] int,
    [encryption_type_desc] nvarchar(64),
    [encryption_algorithm_name] sysname,
    [column_encryption_key_id] int,
    [column_encryption_key_database_name] sysname,
    [is_hidden] bit,
    [is_masked] bit NOT NULL,
    [graph_type] int,
    [graph_type_desc] nvarchar(60),
    [is_data_deletion_filter_column] bit,
    [ledger_view_column_type] int,
    [ledger_view_column_type_desc] nvarchar(60),
    [is_dropped_ledger_column] bit,
    UNIQUE ([object_id], [column_id])
);
GO

CREATE TABLE sys.[configurations] (
    [configuration_id] int NOT NULL,
    [name] nvarchar(35) NOT NULL,
    [value] sql_variant,
    [minimum] sql_variant,
    [maximum] sql_variant,
    [value_in_use] sql_variant,
    [description] nvarchar(255) NOT NULL,
    [is_dynamic] bit NOT NULL,
    [is_advanced] bit NOT NULL,
    UNIQUE ([configuration_id]),
    UNIQUE ([name])
);
GO

CREATE TABLE sys.[data_spaces] (
    [name] sysname NOT NULL,
    [data_space_id] int NOT NULL,
    [type] char(2) NOT NULL,
    [type_desc] nvarchar(60),
    [is_default] bit NOT NULL,
    [is_system] bit
);
GO

CREATE TABLE sys.[database_files] (
    [file_id] int NOT NULL,
    [file_guid] uniqueidentifier,
    [type] tinyint NOT NULL,
    [type_desc] nvarchar(60),
    [data_space_id] int NOT NULL,
    [name] nvarchar(128),
    [physical_name] nvarchar(260),
    [state] tinyint,
    [state_desc] nvarchar(60),
    [size] int NOT NULL,
    [max_size] int NOT NULL,
    [growth] int NOT NULL,
    [is_media_read_only] bit NOT NULL,
    [is_read_only] bit NOT NULL,
    [is_sparse] bit NOT NULL,
    [is_percent_growth] bit NOT NULL,
    [is_name_reserved] bit NOT NULL,
    [is_persistent_log_buffer] bit NOT NULL,
    [create_lsn] numeric(25,0),
    [drop_lsn] numeric(25,0),
    [read_only_lsn] numeric(25,0),
    [read_write_lsn] numeric(25,0),
    [differential_base_lsn] numeric(25,0),
    [differential_base_guid] uniqueidentifier,
    [differential_base_time] datetime,
    [redo_start_lsn] numeric(25,0),
    [redo_start_fork_guid] uniqueidentifier,
    [redo_target_lsn] numeric(25,0),
    [redo_target_fork_guid] uniqueidentifier,
    [backup_lsn] numeric(25,0)
);
GO

CREATE TABLE sys.[database_principals] (
    [name] sysname NOT NULL,
    [principal_id] int NOT NULL,
    [type] char(1) NOT NULL,
    [type_desc] nvarchar(60),
    [default_schema_name] sysname,
    [create_date] datetime NOT NULL,
    [modify_date] datetime NOT NULL,
    [owning_principal_id] int,
    [sid] varbinary(85),
    [is_fixed_role] bit NOT NULL,
    [authentication_type] int NOT NULL,
    [authentication_type_desc] nvarchar(60),
    [default_language_name] sysname,
    [default_language_lcid] int,
    [allow_encrypted_value_modifications] bit NOT NULL,
    [tenant_id] uniqueidentifier,
    UNIQUE ([principal_id]),
    UNIQUE ([name])
);
GO

CREATE TABLE sys.[database_role_members] (
    [role_principal_id] int NOT NULL,
    [member_principal_id] int NOT NULL,
    UNIQUE ([role_principal_id], [member_principal_id])
);
GO

CREATE TABLE sys.[databases] (
    [name] sysname NOT NULL,
    [database_id] int NOT NULL,
    [source_database_id] int,
    [owner_sid] varbinary(85),
    [create_date] datetime NOT NULL,
    [compatibility_level] tinyint NOT NULL,
    [collation_name] sysname,
    [user_access] tinyint,
    [user_access_desc] nvarchar(60),
    [is_read_only] bit,
    [is_auto_close_on] bit NOT NULL,
    [is_auto_shrink_on] bit,
    [state] tinyint,
    [state_desc] nvarchar(60),
    [is_in_standby] bit,
    [is_cleanly_shutdown] bit,
    [is_supplemental_logging_enabled] bit,
    [snapshot_isolation_state] tinyint,
    [snapshot_isolation_state_desc] nvarchar(60),
    [is_read_committed_snapshot_on] bit,
    [recovery_model] tinyint,
    [recovery_model_desc] nvarchar(60),
    [page_verify_option] tinyint,
    [page_verify_option_desc] nvarchar(60),
    [is_auto_create_stats_on] bit,
    [is_auto_create_stats_incremental_on] bit,
    [is_auto_update_stats_on] bit,
    [is_auto_update_stats_async_on] bit,
    [is_ansi_null_default_on] bit,
    [is_ansi_nulls_on] bit,
    [is_ansi_padding_on] bit,
    [is_ansi_warnings_on] bit,
    [is_arithabort_on] bit,
    [is_concat_null_yields_null_on] bit,
    [is_numeric_roundabort_on] bit,
    [is_quoted_identifier_on] bit,
    [is_recursive_triggers_on] bit,
    [is_cursor_close_on_commit_on] bit,
    [is_local_cursor_default] bit,
    [is_fulltext_enabled] bit,
    [is_trustworthy_on] bit,
    [is_db_chaining_on] bit,
    [is_parameterization_forced] bit,
    [is_master_key_encrypted_by_server] bit NOT NULL,
    [is_query_store_on] bit,
    [is_published] bit NOT NULL,
    [is_subscribed] bit NOT NULL,
    [is_merge_published] bit NOT NULL,
    [is_distributor] bit NOT NULL,
    [is_sync_with_backup] bit NOT NULL,
    [service_broker_guid] uniqueidentifier NOT NULL,
    [is_broker_enabled] bit NOT NULL,
    [log_reuse_wait] tinyint,
    [log_reuse_wait_desc] nvarchar(60),
    [is_date_correlation_on] bit NOT NULL,
    [is_cdc_enabled] bit NOT NULL,
    [is_encrypted] bit,
    [is_honor_broker_priority_on] bit,
    [replica_id] uniqueidentifier,
    [group_database_id] uniqueidentifier,
    [resource_pool_id] int,
    [default_language_lcid] smallint,
    [default_language_name] nvarchar(128),
    [default_fulltext_language_lcid] int,
    [default_fulltext_language_name] nvarchar(128),
    [is_nested_triggers_on] bit,
    [is_transform_noise_words_on] bit,
    [two_digit_year_cutoff] smallint,
    [containment] tinyint,
    [containment_desc] nvarchar(60),
    [target_recovery_time_in_seconds] int,
    [delayed_durability] int,
    [delayed_durability_desc] nvarchar(60),
    [is_memory_optimized_elevate_to_snapshot_on] bit,
    [is_federation_member] bit,
    [is_remote_data_archive_enabled] bit,
    [is_mixed_page_allocation_on] bit,
    [is_temporal_history_retention_enabled] bit,
    [catalog_collation_type] int NOT NULL,
    [catalog_collation_type_desc] nvarchar(60),
    [physical_database_name] nvarchar(128),
    [is_result_set_caching_on] bit,
    [is_accelerated_database_recovery_on] bit,
    [is_tempdb_spill_to_remote_store] bit,
    [is_stale_page_detection_on] bit,
    [is_memory_optimized_enabled] bit,
    [is_data_retention_enabled] bit,
    [is_ledger_on] bit,
    [is_change_feed_enabled] bit,
    UNIQUE ([database_id]),
    UNIQUE ([name])
);
GO

CREATE TABLE sys.[default_constraints] (
    [name] sysname NOT NULL,
    [object_id] int NOT NULL,
    [principal_id] int,
    [schema_id] int NOT NULL,
    [parent_object_id] int NOT NULL,
    [type] char(2),
    [type_desc] nvarchar(60),
    [create_date] datetime NOT NULL,
    [modify_date] datetime NOT NULL,
    [is_ms_shipped] bit NOT NULL,
    [is_published] bit NOT NULL,
    [is_schema_published] bit NOT NULL,
    [parent_column_id] int NOT NULL,
    [definition] nvarchar(max),
    [is_system_named] bit NOT NULL,
    UNIQUE ([object_id])
);
GO

CREATE TABLE sys.[destination_data_spaces] (
    [partition_scheme_id] int NOT NULL,
    [destination_id] int NOT NULL,
    [data_space_id] int NOT NULL
);
GO

CREATE TABLE sys.[external_data_sources] (
    [data_source_id] int NOT NULL,
    [name] nvarchar(128) NOT NULL,
    [location] nvarchar(4000) NOT NULL,
    [type_desc] nvarchar(255),
    [type] tinyint NOT NULL,
    [resource_manager_location] nvarchar(4000),
    [credential_id] int NOT NULL,
    [database_name] nvarchar(128),
    [shard_map_name] nvarchar(128),
    [connection_options] nvarchar(4000),
    [pushdown] nvarchar(256) NOT NULL
);
GO

CREATE TABLE sys.[external_file_formats] (
    [file_format_id] int NOT NULL,
    [name] nvarchar(128) NOT NULL,
    [format_type] nvarchar(100) NOT NULL,
    [field_terminator] nvarchar(10),
    [string_delimiter] nvarchar(10),
    [date_format] nvarchar(50),
    [use_type_default] bit,
    [serde_method] nvarchar(255),
    [row_terminator] nvarchar(10),
    [encoding] nvarchar(10),
    [data_compression] nvarchar(255),
    [first_row] int,
    [parser_version] nvarchar(32)
);
GO

CREATE TABLE sys.[external_tables] (
    [name] sysname NOT NULL,
    [object_id] int NOT NULL,
    [principal_id] int,
    [schema_id] int NOT NULL,
    [parent_object_id] int NOT NULL,
    [type] char(2),
    [type_desc] nvarchar(60),
    [create_date] datetime NOT NULL,
    [modify_date] datetime NOT NULL,
    [is_ms_shipped] bit NOT NULL,
    [is_published] bit NOT NULL,
    [is_schema_published] bit NOT NULL,
    [max_column_id_used] int,
    [uses_ansi_nulls] bit,
    [data_source_id] int NOT NULL,
    [file_format_id] int,
    [location] nvarchar(4000),
    [reject_type] nvarchar(20),
    [reject_value] float,
    [reject_sample_value] float,
    [distribution_type] tinyint,
    [distribution_desc] nvarchar(120),
    [sharding_col_id] int,
    [remote_schema_name] nvarchar(128),
    [remote_object_name] nvarchar(128),
    [rejected_row_location] nvarchar(4000),
    [table_options] nvarchar(1000),
    [partition_type] int NOT NULL,
    [partition_desc] nvarchar(60),
    UNIQUE ([object_id])
);
GO

CREATE TABLE sys.[filegroups] (
    [name] sysname NOT NULL,
    [data_space_id] int NOT NULL,
    [type] char(2) NOT NULL,
    [type_desc] nvarchar(60),
    [is_default] bit,
    [is_system] bit,
    [filegroup_guid] uniqueidentifier,
    [log_filegroup_id] int,
    [is_read_only] bit,
    [is_autogrow_all_files] bit
);
GO

CREATE TABLE sys.[foreign_keys] (
    [name] sysname NOT NULL,
    [object_id] int NOT NULL,
    [principal_id] int,
    [schema_id] int NOT NULL,
    [parent_object_id] int NOT NULL,
    [type] char(2),
    [type_desc] nvarchar(60),
    [create_date] datetime NOT NULL,
    [modify_date] datetime NOT NULL,
    [is_ms_shipped] bit NOT NULL,
    [is_published] bit NOT NULL,
    [is_schema_published] bit NOT NULL,
    [referenced_object_id] int,
    [key_index_id] int,
    [is_disabled] bit NOT NULL,
    [is_not_for_replication] bit NOT NULL,
    [is_not_trusted] bit NOT NULL,
    [delete_referential_action] tinyint,
    [delete_referential_action_desc] nvarchar(60),
    [update_referential_action] tinyint,
    [update_referential_action_desc] nvarchar(60),
    [is_system_named] bit NOT NULL,
    UNIQUE ([object_id])
);
GO

CREATE TABLE sys.[index_columns] (
    [object_id] int NOT NULL,
    [index_id] int NOT NULL,
    [index_column_id] int NOT NULL,
    [column_id] int NOT NULL,
    [key_ordinal] tinyint NOT NULL,
    [partition_ordinal] tinyint NOT NULL,
    [is_descending_key] bit,
    [is_included_column] bit,
    [column_store_order_ordinal] tinyint NOT NULL,
    UNIQUE ([object_id], [index_id], [index_column_id])
);
GO

CREATE TABLE sys.[indexes] (
    [object_id] int NOT NULL,
    [name] sysname,
    [index_id] int NOT NULL,
    [type] tinyint NOT NULL,
    [type_desc] nvarchar(60),
    [is_unique] bit,
    [data_space_id] int,
    [ignore_dup_key] bit,
    [is_primary_key] bit,
    [is_unique_constraint] bit,
    [fill_factor] tinyint NOT NULL,
    [is_padded] bit,
    [is_disabled] bit,
    [is_hypothetical] bit,
    [is_ignored_in_optimization] bit,
    [allow_row_locks] bit,
    [allow_page_locks] bit,
    [has_filter] bit,
    [filter_definition] nvarchar(max),
    [compression_delay] int,
    [suppress_dup_key_messages] bit,
    [auto_created] bit,
    [optimize_for_sequential_key] bit,
    UNIQUE ([object_id], [index_id])
);
GO

CREATE TABLE sys.[key_constraints] (
    [name] sysname NOT NULL,
    [object_id] int NOT NULL,
    [principal_id] int,
    [schema_id] int NOT NULL,
    [parent_object_id] int NOT NULL,
    [type] char(2),
    [type_desc] nvarchar(60),
    [create_date] datetime NOT NULL,
    [modify_date] datetime NOT NULL,
    [is_ms_shipped] bit NOT NULL,
    [is_published] bit NOT NULL,
    [is_schema_published] bit NOT NULL,
    [unique_index_id] int,
    [is_system_named] bit NOT NULL,
    [is_enforced] bit,
    UNIQUE ([object_id])
);
GO

CREATE TABLE sys.[master_files] (
    [database_id] int NOT NULL,
    [file_id] int NOT NULL,
    [file_guid] uniqueidentifier,
    [type] tinyint NOT NULL,
    [type_desc] nvarchar(60),
    [data_space_id] int NOT NULL,
    [name] nvarchar(128),
    [physical_name] nvarchar(260) NOT NULL,
    [state] tinyint,
    [state_desc] nvarchar(60),
    [size] int NOT NULL,
    [max_size] int NOT NULL,
    [growth] int NOT NULL,
    [is_media_read_only] bit NOT NULL,
    [is_read_only] bit NOT NULL,
    [is_sparse] bit NOT NULL,
    [is_percent_growth] bit NOT NULL,
    [is_name_reserved] bit NOT NULL,
    [is_persistent_log_buffer] bit NOT NULL,
    [create_lsn] numeric(25,0),
    [drop_lsn] numeric(25,0),
    [read_only_lsn] numeric(25,0),
    [read_write_lsn] numeric(25,0),
    [differential_base_lsn] numeric(25,0),
    [differential_base_guid] uniqueidentifier,
    [differential_base_time] datetime,
    [redo_start_lsn] numeric(25,0),
    [redo_start_fork_guid] uniqueidentifier,
    [redo_target_lsn] numeric(25,0),
    [redo_target_fork_guid] uniqueidentifier,
    [backup_lsn] numeric(25,0),
    [credential_id] int
);
GO

CREATE TABLE sys.[objects] (
    [name] sysname NOT NULL,
    [object_id] int NOT NULL,
    [principal_id] int,
    [schema_id] int NOT NULL,
    [parent_object_id] int NOT NULL,
    [type] char(2),
    [type_desc] nvarchar(60),
    [create_date] datetime NOT NULL,
    [modify_date] datetime NOT NULL,
    [is_ms_shipped] bit NOT NULL,
    [is_published] bit NOT NULL,
    [is_schema_published] bit NOT NULL,
    UNIQUE ([object_id])
);
GO

CREATE TABLE sys.[partition_schemes] (
    [name] sysname NOT NULL,
    [data_space_id] int NOT NULL,
    [type] char(2) NOT NULL,
    [type_desc] nvarchar(60),
    [is_default] bit,
    [is_system] bit,
    [function_id] int NOT NULL
);
GO

CREATE TABLE sys.[partitions] (
    [partition_id] bigint NOT NULL,
    [object_id] int NOT NULL,
    [index_id] int NOT NULL,
    [partition_number] int NOT NULL,
    [hobt_id] bigint NOT NULL,
    [rows] bigint,
    [filestream_filegroup_id] smallint NOT NULL,
    [data_compression] tinyint NOT NULL,
    [data_compression_desc] nvarchar(60),
    [xml_compression] bit,
    [xml_compression_desc] varchar(3),
    UNIQUE ([partition_id])
);
GO

CREATE TABLE sys.[procedures] (
    [name] sysname NOT NULL,
    [object_id] int NOT NULL,
    [principal_id] int,
    [schema_id] int NOT NULL,
    [parent_object_id] int NOT NULL,
    [type] char(2),
    [type_desc] nvarchar(60),
    [create_date] datetime NOT NULL,
    [modify_date] datetime NOT NULL,
    [is_ms_shipped] bit NOT NULL,
    [is_published] bit NOT NULL,
    [is_schema_published] bit NOT NULL,
    [is_auto_executed] bit NOT NULL,
    [is_execution_replicated] bit,
    [is_repl_serializable_only] bit,
    [skips_repl_constraints] bit,
    UNIQUE ([object_id])
);
GO

CREATE TABLE sys.[schemas] (
    [name] sysname NOT NULL,
    [schema_id] int NOT NULL,
    [principal_id] int,
    UNIQUE ([schema_id]),
    UNIQUE ([name])
);
GO

CREATE TABLE sys.[sequences] (
    [name] sysname NOT NULL,
    [object_id] int NOT NULL,
    [principal_id] int,
    [schema_id] int NOT NULL,
    [parent_object_id] int NOT NULL,
    [type] char(2),
    [type_desc] nvarchar(60),
    [create_date] datetime NOT NULL,
    [modify_date] datetime NOT NULL,
    [is_ms_shipped] bit NOT NULL,
    [is_published] bit NOT NULL,
    [is_schema_published] bit NOT NULL,
    [start_value] sql_variant NOT NULL,
    [increment] sql_variant NOT NULL,
    [minimum_value] sql_variant NOT NULL,
    [maximum_value] sql_variant NOT NULL,
    [is_cycling] bit,
    [is_cached] bit,
    [cache_size] int,
    [system_type_id] tinyint NOT NULL,
    [user_type_id] int NOT NULL,
    [precision] tinyint NOT NULL,
    [scale] tinyint,
    [current_value] sql_variant NOT NULL,
    [is_exhausted] bit NOT NULL,
    [last_used_value] sql_variant,
    UNIQUE ([object_id])
);
GO

CREATE TABLE sys.[server_principals] (
    [name] sysname NOT NULL,
    [principal_id] int NOT NULL,
    [sid] varbinary(85),
    [type] char(1) NOT NULL,
    [type_desc] nvarchar(60),
    [is_disabled] bit,
    [create_date] datetime NOT NULL,
    [modify_date] datetime NOT NULL,
    [default_database_name] sysname,
    [default_language_name] sysname,
    [credential_id] int,
    [owning_principal_id] int,
    [is_fixed_role] bit NOT NULL,
    [tenant_id] uniqueidentifier,
    UNIQUE ([principal_id]),
    UNIQUE ([name])
);
GO

CREATE TABLE sys.[server_role_members] (
    [role_principal_id] int NOT NULL,
    [member_principal_id] int NOT NULL
);
GO

CREATE TABLE sys.[sql_modules] (
    [object_id] int NOT NULL,
    [definition] nvarchar(max),
    [uses_ansi_nulls] bit,
    [uses_quoted_identifier] bit,
    [is_schema_bound] bit,
    [uses_database_collation] bit,
    [is_recompiled] bit,
    [null_on_null_input] bit,
    [execute_as_principal_id] int,
    [uses_native_compilation] bit,
    [inline_type] bit,
    [is_inlineable] bit,
    UNIQUE ([object_id])
);
GO

CREATE TABLE sys.[stats] (
    [object_id] int NOT NULL,
    [name] nvarchar(128),
    [stats_id] int NOT NULL,
    [auto_created] bit,
    [user_created] bit,
    [no_recompute] bit,
    [has_filter] bit,
    [filter_definition] nvarchar(max),
    [is_temporary] bit,
    [is_incremental] bit,
    [has_persisted_sample] bit,
    [stats_generation_method] int NOT NULL,
    [stats_generation_method_desc] varchar(80) NOT NULL,
    [auto_drop] bit,
    UNIQUE ([object_id], [stats_id])
);
GO

CREATE TABLE sys.[stats_columns] (
    [object_id] int NOT NULL,
    [stats_id] int NOT NULL,
    [stats_column_id] int,
    [column_id] int
);
GO

CREATE TABLE sys.[synonyms] (
    [name] sysname NOT NULL,
    [object_id] int NOT NULL,
    [principal_id] int,
    [schema_id] int NOT NULL,
    [parent_object_id] int NOT NULL,
    [type] char(2),
    [type_desc] nvarchar(60),
    [create_date] datetime NOT NULL,
    [modify_date] datetime NOT NULL,
    [is_ms_shipped] bit NOT NULL,
    [is_published] bit NOT NULL,
    [is_schema_published] bit NOT NULL,
    [base_object_name] nvarchar(1035),
    UNIQUE ([object_id])
);
GO

CREATE TABLE sys.[tables] (
    [name] sysname NOT NULL,
    [object_id] int NOT NULL,
    [principal_id] int,
    [schema_id] int NOT NULL,
    [parent_object_id] int NOT NULL,
    [type] char(2),
    [type_desc] nvarchar(60),
    [create_date] datetime NOT NULL,
    [modify_date] datetime NOT NULL,
    [is_ms_shipped] bit NOT NULL,
    [is_published] bit NOT NULL,
    [is_schema_published] bit NOT NULL,
    [lob_data_space_id] int NOT NULL,
    [filestream_data_space_id] int,
    [max_column_id_used] int NOT NULL,
    [lock_on_bulk_load] bit NOT NULL,
    [uses_ansi_nulls] bit,
    [is_replicated] bit,
    [has_replication_filter] bit,
    [is_merge_published] bit,
    [is_sync_tran_subscribed] bit,
    [has_unchecked_assembly_data] bit NOT NULL,
    [text_in_row_limit] int,
    [large_value_types_out_of_row] bit,
    [is_tracked_by_cdc] bit,
    [lock_escalation] tinyint,
    [lock_escalation_desc] nvarchar(60),
    [is_filetable] bit,
    [is_memory_optimized] bit,
    [durability] tinyint,
    [durability_desc] nvarchar(60),
    [temporal_type] tinyint,
    [temporal_type_desc] nvarchar(60),
    [history_table_id] int,
    [is_remote_data_archive_enabled] bit,
    [is_external] bit NOT NULL,
    [history_retention_period] int,
    [history_retention_period_unit] int,
    [history_retention_period_unit_desc] nvarchar(10),
    [is_node] bit,
    [is_edge] bit,
    [data_retention_period] int,
    [data_retention_period_unit] int,
    [data_retention_period_unit_desc] nvarchar(10),
    [ledger_type] tinyint,
    [ledger_type_desc] nvarchar(60),
    [ledger_view_id] int,
    [is_dropped_ledger_table] bit,
    UNIQUE ([object_id])
);
GO

CREATE TABLE sys.[triggers] (
    [name] sysname NOT NULL,
    [object_id] int NOT NULL,
    [parent_class] tinyint NOT NULL,
    [parent_class_desc] nvarchar(60),
    [parent_id] int NOT NULL,
    [type] char(2) NOT NULL,
    [type_desc] nvarchar(60),
    [create_date] datetime NOT NULL,
    [modify_date] datetime NOT NULL,
    [is_ms_shipped] bit NOT NULL,
    [is_disabled] bit NOT NULL,
    [is_not_for_replication] bit NOT NULL,
    [is_instead_of_trigger] bit NOT NULL,
    UNIQUE ([object_id])
);
GO

CREATE TABLE sys.[types] (
    [name] sysname NOT NULL,
    [system_type_id] tinyint NOT NULL,
    [user_type_id] int NOT NULL,
    [schema_id] int NOT NULL,
    [principal_id] int,
    [max_length] smallint NOT NULL,
    [precision] tinyint NOT NULL,
    [scale] tinyint NOT NULL,
    [collation_name] sysname,
    [is_nullable] bit,
    [is_user_defined] bit NOT NULL,
    [is_assembly_type] bit NOT NULL,
    [default_object_id] int NOT NULL,
    [rule_object_id] int NOT NULL,
    [is_table_type] bit NOT NULL,
    UNIQUE ([user_type_id])
);
GO

CREATE TABLE sys.[views] (
    [name] sysname NOT NULL,
    [object_id] int NOT NULL,
    [principal_id] int,
    [schema_id] int NOT NULL,
    [parent_object_id] int NOT NULL,
    [type] char(2),
    [type_desc] nvarchar(60),
    [create_date] datetime NOT NULL,
    [modify_date] datetime NOT NULL,
    [is_ms_shipped] bit NOT NULL,
    [is_published] bit NOT NULL,
    [is_schema_published] bit NOT NULL,
    [is_replicated] bit,
    [has_replication_filter] bit,
    [has_opaque_metadata] bit NOT NULL,
    [has_unchecked_assembly_data] bit NOT NULL,
    [with_check_option] bit NOT NULL,
    [is_date_correlation_view] bit NOT NULL,
    [is_tracked_by_cdc] bit,
    [has_snapshot] bit,
    [ledger_view_type] tinyint,
    [ledger_view_type_desc] nvarchar(60),
    [is_dropped_ledger_view] bit,
    UNIQUE ([object_id])
);
GO
