//! A table's full structure (columns, indexes, foreign keys, checks, triggers), read from each engine's catalog.

use crate::db::{Conn, Dialect};
use crate::error::{Error, Result};
use serde::Serialize;

/// Separates list items inside one catalog cell (the ASCII unit separator, which no identifier contains).
const SEP: char = '\u{1f}';

#[derive(Debug, Serialize, PartialEq)]
pub struct Structure {
    pub columns: Vec<ColumnInfo>,
    pub indexes: Vec<IndexInfo>,
    pub foreign_keys: Vec<ForeignKeyInfo>,
    pub checks: Vec<NamedDef>,
    pub triggers: Vec<TriggerInfo>,
    pub comment: Option<String>,
    /// MySQL's engine and collation; None elsewhere.
    pub options: Option<TableOptions>,
}

/// A MySQL table's engine and default collation, with the ones the server offers to choose from.
#[derive(Debug, Serialize, PartialEq)]
pub struct TableOptions {
    pub engine: Option<String>,
    pub collation: Option<String>,
    pub engines: Vec<String>,
    pub collations: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq, Default)]
#[expect(clippy::struct_excessive_bools, reason = "each is a separate fact the catalog gives about the column")]
pub struct ColumnInfo {
    pub name: String,
    /// The full type as the database writes it, e.g. `character varying(255)` or `int unsigned`.
    pub data_type: String,
    pub nullable: bool,
    /// The default as an SQL expression, or None.
    pub default: Option<String>,
    pub comment: Option<String>,
    pub auto_increment: bool,
    pub primary: bool,
    /// MySQL's `ON UPDATE CURRENT_TIMESTAMP`, kept so changing the column doesn't drop it.
    #[serde(skip)]
    pub on_update: Option<String>,
    /// MySQL's collation for a text column, kept when the column is restated (it would fall back to the table's).
    #[serde(skip)]
    pub collation: Option<String>,
    /// A MySQL generated column, which restating would turn into a plain one.
    #[serde(skip)]
    pub generated: bool,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct IndexInfo {
    pub name: String,
    /// Column names, or expressions for expression indexes.
    pub columns: Vec<String>,
    pub unique: bool,
    pub primary: bool,
    /// btree, hash, gin… where the engine has a choice.
    pub method: Option<String>,
    /// Backs a constraint (primary key, or a Postgres unique constraint), so it's dropped as the constraint.
    #[serde(skip)]
    pub constraint: bool,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct ForeignKeyInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    /// `NO ACTION`, `RESTRICT`, `CASCADE`, `SET NULL` or `SET DEFAULT`.
    pub on_update: String,
    pub on_delete: String,
}

/// A trigger: its definition as SQL, and its parts for the structure editor when it can restate them (a row-level
/// trigger without a WHEN clause or column list; otherwise `timing` is None and it can only be dropped).
#[derive(Debug, Serialize, PartialEq, Default)]
pub struct TriggerInfo {
    pub name: String,
    pub definition: String,
    /// `BEFORE` or `AFTER`.
    pub timing: Option<String>,
    /// `INSERT`, `UPDATE`, `DELETE`; Postgres can fire on several.
    pub events: Vec<String>,
    /// What it runs: MySQL's and SQLite's statements, or Postgres's function call.
    pub body: Option<String>,
}

/// A trigger's parts from the catalog's row: name, definition, timing, events (SEP-separated), body.
fn trigger(mut r: Vec<Option<String>>) -> TriggerInfo {
    let timing = take(&mut r, 2).filter(|t| t == "BEFORE" || t == "AFTER");
    let body = take(&mut r, 4).filter(|_| timing.is_some());
    TriggerInfo {
        name: take(&mut r, 0).unwrap_or_default(),
        definition: take(&mut r, 1).unwrap_or_default(),
        events: if body.is_some() { list(take(&mut r, 3)) } else { vec![] },
        timing: timing.filter(|_| body.is_some()),
        body,
    }
}

/// SQLite keeps only a trigger's CREATE statement: its parts, read back from it, when it's a plain row trigger
/// (`BEFORE|AFTER INSERT|UPDATE|DELETE ON t [FOR EACH ROW] BEGIN … END`). The last timing and event words before ` ON `
/// count, since the trigger's name comes before them.
fn lite_trigger(name: String, sql: String) -> TriggerInfo {
    let upper = sql.to_ascii_uppercase();
    let parts = (|| {
        let head = &upper[..upper.find(" ON ")?];
        let begin = upper.find("BEGIN")?;
        let end = upper.rfind("END")?;
        let words: Vec<&str> = head.split_whitespace().collect();
        if upper[..begin].contains(" WHEN ") || head.contains(" OF ") || end <= begin {
            return None;
        }
        let timing = words.iter().rev().find(|w| ["BEFORE", "AFTER"].contains(w)).map_or("BEFORE", |w| w);
        let event = words.iter().rev().find(|w| ["INSERT", "UPDATE", "DELETE"].contains(w))?;
        let body = sql[begin + "BEGIN".len()..end].trim();
        Some((timing.to_string(), (*event).to_string(), body.to_string()))
    })();
    match parts {
        Some((timing, event, body)) => {
            TriggerInfo { name, definition: sql, timing: Some(timing), events: vec![event], body: Some(body) }
        }
        None => TriggerInfo { name, definition: sql, ..TriggerInfo::default() },
    }
}

/// A check constraint: its name and its definition as SQL.
#[derive(Debug, Serialize, PartialEq)]
pub struct NamedDef {
    pub name: String,
    pub definition: String,
}

fn list(cell: Option<String>) -> Vec<String> {
    cell.map(|c| c.split(SEP).map(String::from).filter(|s| !s.is_empty()).collect()).unwrap_or_default()
}

/// Postgres's one-letter referential action codes.
fn pg_action(code: Option<&str>) -> String {
    match code {
        Some("r") => "RESTRICT",
        Some("c") => "CASCADE",
        Some("n") => "SET NULL",
        Some("d") => "SET DEFAULT",
        _ => "NO ACTION",
    }
    .into()
}

/// A MySQL column default as an SQL expression. MariaDB already writes it that way (with `NULL` for none);
/// MySQL gives literals unquoted and marks expressions `DEFAULT_GENERATED` in `extra`.
fn my_default(maria: bool, data_type: &str, extra: &str, default: String) -> Option<String> {
    const NUMERIC: [&str; 9] =
        ["tinyint", "smallint", "mediumint", "int", "bigint", "decimal", "float", "double", "bit"];
    if maria {
        return (default != "NULL").then_some(default);
    }
    let generated = extra.contains("DEFAULT_GENERATED");
    let numeric = NUMERIC.iter().any(|t| data_type.starts_with(t));
    Some(if generated && default.to_ascii_uppercase().starts_with("CURRENT_TIMESTAMP") {
        default
    } else if generated {
        format!("({default})")
    } else if numeric {
        default // numbers, and bit literals such as b'1'
    } else {
        Dialect::My.literal(&default)
    })
}

/// A MySQL `information_schema.columns` row: name, type, nullable, default, comment, extra, key, collation.
fn my_column(maria: bool, mut r: Vec<Option<String>>) -> ColumnInfo {
    let data_type = take(&mut r, 1).unwrap_or_default();
    let extra = take(&mut r, 5).unwrap_or_default();
    let default = take(&mut r, 3).and_then(|d| my_default(maria, &data_type, &extra, d));
    ColumnInfo {
        name: take(&mut r, 0).unwrap_or_default(),
        nullable: take(&mut r, 2).as_deref() == Some("YES"),
        default,
        comment: take(&mut r, 4).filter(|c| !c.is_empty()),
        auto_increment: extra.contains("auto_increment"),
        primary: take(&mut r, 6).as_deref() == Some("PRI"),
        on_update: extra.to_ascii_lowercase().find("on update ").map(|i| extra[i + 10..].to_string()),
        collation: take(&mut r, 7),
        generated: extra.to_ascii_uppercase().contains("GENERATED"),
        data_type,
    }
}

/// Cell `i` of a catalog row, taken out (rows are read once, front to back).
fn take(row: &mut [Option<String>], i: usize) -> Option<String> {
    row.get_mut(i).and_then(Option::take)
}

impl Conn {
    /// The table's structure. `table` is `schema.table` for Postgres (`public` when there's no schema).
    pub async fn structure(&mut self, table: &str) -> Result<Structure> {
        let s = match self.dialect() {
            Dialect::Pg => self.pg_structure(table).await?,
            Dialect::My => self.my_structure(table).await?,
            Dialect::Lite => self.lite_structure(table).await?,
        };
        if s.columns.is_empty() {
            return Err(Error::Invalid("no such table, or it has no columns"));
        }
        Ok(s)
    }

    async fn pg_structure(&mut self, table: &str) -> Result<Structure> {
        let (schema, name) = table.split_once('.').unwrap_or(("public", table));
        let oid_rows = self
            .fetch_with(
                "select c.oid::text, obj_description(c.oid, 'pg_class') from pg_class c \
                 join pg_namespace n on n.oid = c.relnamespace where n.nspname = $1 and c.relname = $2",
                &[Some(schema), Some(name)],
            )
            .await?;
        let Some(mut first) = oid_rows.into_iter().next() else { return Err(Error::Invalid("no such table")) };
        let oid = take(&mut first, 0).unwrap_or_default();
        let comment = take(&mut first, 1);
        let oid_param = [Some(oid.as_str())];

        let columns = self
            .fetch_with(
                "select a.attname::text, format_type(a.atttypid, a.atttypmod), (not a.attnotnull)::text, \
                 pg_get_expr(d.adbin, d.adrelid), col_description(a.attrelid, a.attnum), \
                 (a.attidentity <> '' or coalesce(pg_get_expr(d.adbin, d.adrelid), '') like 'nextval(%')::text, \
                 coalesce(a.attnum = any(p.conkey), false)::text \
                 from pg_attribute a \
                 left join pg_attrdef d on d.adrelid = a.attrelid and d.adnum = a.attnum \
                 left join pg_constraint p on p.conrelid = a.attrelid and p.contype = 'p' \
                 where a.attrelid = $1::oid and a.attnum > 0 and not a.attisdropped order by a.attnum",
                &oid_param,
            )
            .await?
            .into_iter()
            .map(|mut r| ColumnInfo {
                name: take(&mut r, 0).unwrap_or_default(),
                data_type: take(&mut r, 1).unwrap_or_default(),
                nullable: take(&mut r, 2).as_deref() == Some("true"),
                default: take(&mut r, 3),
                comment: take(&mut r, 4),
                auto_increment: take(&mut r, 5).as_deref() == Some("true"),
                primary: take(&mut r, 6).as_deref() == Some("true"),
                ..ColumnInfo::default()
            })
            .collect();

        let indexes = self
            .fetch_with(
                "select i.relname::text, \
                 array_to_string(array(select pg_get_indexdef(ix.indexrelid, k + 1, true) \
                   from generate_subscripts(ix.indkey, 1) k order by k), chr(31)), \
                 ix.indisunique::text, ix.indisprimary::text, am.amname::text, \
                 exists(select 1 from pg_constraint c where c.conindid = ix.indexrelid and c.contype in ('p', 'u'))::text \
                 from pg_index ix join pg_class i on i.oid = ix.indexrelid join pg_am am on am.oid = i.relam \
                 where ix.indrelid = $1::oid order by ix.indisprimary desc, i.relname",
                &oid_param,
            )
            .await?
            .into_iter()
            .map(|mut r| IndexInfo {
                name: take(&mut r, 0).unwrap_or_default(),
                columns: list(take(&mut r, 1)),
                unique: take(&mut r, 2).as_deref() == Some("true"),
                primary: take(&mut r, 3).as_deref() == Some("true"),
                method: take(&mut r, 4),
                constraint: take(&mut r, 5).as_deref() == Some("true"),
            })
            .collect();

        let foreign_keys = self
            .fetch_with(
                "select c.conname::text, \
                 array_to_string(array(select a.attname from unnest(c.conkey) with ordinality k(n, i) \
                   join pg_attribute a on a.attrelid = c.conrelid and a.attnum = k.n order by k.i), chr(31)), \
                 (select n.nspname || '.' || r.relname from pg_class r join pg_namespace n on n.oid = r.relnamespace \
                   where r.oid = c.confrelid), \
                 array_to_string(array(select a.attname from unnest(c.confkey) with ordinality k(n, i) \
                   join pg_attribute a on a.attrelid = c.confrelid and a.attnum = k.n order by k.i), chr(31)), \
                 c.confupdtype::text, c.confdeltype::text \
                 from pg_constraint c where c.conrelid = $1::oid and c.contype = 'f' order by c.conname",
                &oid_param,
            )
            .await?
            .into_iter()
            .map(|mut r| ForeignKeyInfo {
                name: take(&mut r, 0).unwrap_or_default(),
                columns: list(take(&mut r, 1)),
                ref_table: take(&mut r, 2).unwrap_or_default(),
                ref_columns: list(take(&mut r, 3)),
                on_update: pg_action(take(&mut r, 4).as_deref()),
                on_delete: pg_action(take(&mut r, 5).as_deref()),
            })
            .collect();

        let checks = self
            .named(
                "select conname::text, pg_get_constraintdef(oid) from pg_constraint \
                 where conrelid = $1::oid and contype = 'c' order by conname",
                &oid_param,
            )
            .await?;
        let triggers = self.pg_triggers(&oid_param).await?;
        Ok(Structure { columns, indexes, foreign_keys, checks, triggers, comment, options: None })
    }

    /// A Postgres table's triggers, by its oid. tgtype bits: 1 row-level, 2 before, 4 insert, 8 delete, 16 update,
    /// 64 instead of. The function call is restated only without arguments (they're kept as bytes), and only for a
    /// row-level trigger without WHEN or a column list.
    async fn pg_triggers(&mut self, oid: &[Option<&str>]) -> Result<Vec<TriggerInfo>> {
        let rows = self
            .fetch_with(
                "select tgname::text, pg_get_triggerdef(oid, true), \
                 case when tgtype::int & 1 = 0 or tgqual is not null or tgattr::text <> '' then null \
                      when tgtype::int & 2 <> 0 then 'BEFORE' when tgtype::int & 64 <> 0 then 'INSTEAD OF' else 'AFTER' end, \
                 concat_ws(chr(31), case when tgtype::int & 4 <> 0 then 'INSERT' end, \
                   case when tgtype::int & 16 <> 0 then 'UPDATE' end, case when tgtype::int & 8 <> 0 then 'DELETE' end), \
                 case when tgnargs = 0 then tgfoid::regproc::text || '()' end \
                 from pg_trigger where tgrelid = $1::oid and not tgisinternal order by tgname",
                oid,
            )
            .await;
        let rows = match rows {
            // CockroachDB has no pg_get_triggerdef (its pg_trigger is empty too), so it lists no triggers.
            Err(Error::Postgres(e)) if e.code() == Some(&tokio_postgres::error::SqlState::UNDEFINED_FUNCTION) => {
                return Ok(Vec::new());
            }
            rows => rows?,
        };
        Ok(rows.into_iter().map(trigger).collect())
    }

    async fn my_structure(&mut self, table: &str) -> Result<Structure> {
        let t = [Some(table)];
        let maria = self.server_version().await?.starts_with("MariaDB");
        let columns = self
            .fetch_with(
                "select column_name, column_type, is_nullable, column_default, column_comment, extra, column_key, \
                 collation_name \
                 from information_schema.columns where table_schema = database() and table_name = ? \
                 order by ordinal_position",
                &t,
            )
            .await?
            .into_iter()
            .map(|r| my_column(maria, r))
            .collect();
        let indexes = self
            .fetch_with(
                // Functional indexes (MySQL 8) have no column name; MariaDB has no `expression` column to show instead.
                "select index_name, group_concat(coalesce(column_name, '<expression>') order by seq_in_index \
                 separator 0x1f), min(non_unique), index_type \
                 from information_schema.statistics where table_schema = database() and table_name = ? \
                 group by index_name, index_type order by index_name = 'PRIMARY' desc, index_name",
                &t,
            )
            .await?
            .into_iter()
            .map(|mut r| {
                let name = take(&mut r, 0).unwrap_or_default();
                let primary = name == "PRIMARY";
                IndexInfo {
                    primary,
                    name,
                    columns: list(take(&mut r, 1)),
                    unique: take(&mut r, 2).as_deref() == Some("0"),
                    method: take(&mut r, 3),
                    constraint: primary,
                }
            })
            .collect();
        let foreign_keys = self
            .fetch_with(
                "select k.constraint_name, group_concat(k.column_name order by k.ordinal_position separator 0x1f), \
                 min(k.referenced_table_name), \
                 group_concat(k.referenced_column_name order by k.ordinal_position separator 0x1f), \
                 min(r.update_rule), min(r.delete_rule) \
                 from information_schema.key_column_usage k \
                 join information_schema.referential_constraints r \
                   on r.constraint_schema = k.constraint_schema and r.constraint_name = k.constraint_name \
                 where k.table_schema = database() and k.table_name = ? and k.referenced_table_name is not null \
                 group by k.constraint_name order by k.constraint_name",
                &t,
            )
            .await?
            .into_iter()
            .map(|mut r| ForeignKeyInfo {
                name: take(&mut r, 0).unwrap_or_default(),
                columns: list(take(&mut r, 1)),
                ref_table: take(&mut r, 2).unwrap_or_default(),
                ref_columns: list(take(&mut r, 3)),
                on_update: take(&mut r, 4).unwrap_or_else(|| "NO ACTION".into()),
                on_delete: take(&mut r, 5).unwrap_or_else(|| "NO ACTION".into()),
            })
            .collect();
        // CHECK constraints are listed from MySQL 8.0.16 and MariaDB 10.2; older servers have no such table.
        let checks = self
            .named(
                "select c.constraint_name, c.check_clause from information_schema.check_constraints c \
                 join information_schema.table_constraints t \
                   on t.constraint_schema = c.constraint_schema and t.constraint_name = c.constraint_name \
                 where t.table_schema = database() and t.table_name = ? and t.constraint_type = 'CHECK' \
                 order by c.constraint_name",
                &t,
            )
            .await
            .unwrap_or_default();
        let triggers = self
            .fetch_with(
                "select trigger_name, concat(action_timing, ' ', event_manipulation, ' FOR EACH ', \
                 action_orientation, ' ', action_statement), action_timing, event_manipulation, action_statement \
                 from information_schema.triggers \
                 where event_object_schema = database() and event_object_table = ? order by trigger_name",
                &t,
            )
            .await?
            .into_iter()
            .map(trigger)
            .collect();
        let (comment, options) = self.my_table_options(table).await?;
        Ok(Structure { columns, indexes, foreign_keys, checks, triggers, comment, options: Some(options) })
    }

    /// A MySQL table's comment, engine and collation, with the engines and collations the server offers.
    async fn my_table_options(&mut self, table: &str) -> Result<(Option<String>, TableOptions)> {
        let mut row = self
            .fetch_with(
                "select table_comment, engine, table_collation from information_schema.tables \
                 where table_schema = database() and table_name = ?",
                &[Some(table)],
            )
            .await?
            .into_iter()
            .next()
            .unwrap_or_default();
        let names = |rows: Vec<Vec<Option<String>>>| rows.into_iter().filter_map(|mut r| take(&mut r, 0)).collect();
        let engines = self
            .fetch_with(
                "select engine from information_schema.engines where support in ('YES', 'DEFAULT') order by engine",
                &[],
            )
            .await?;
        let collations = self
            .fetch_with("select collation_name from information_schema.collations order by collation_name", &[])
            .await?;
        let options = TableOptions {
            engine: take(&mut row, 1),
            collation: take(&mut row, 2),
            engines: names(engines),
            collations: names(collations),
        };
        Ok((take(&mut row, 0).filter(|c| !c.is_empty()), options))
    }

    async fn lite_structure(&mut self, table: &str) -> Result<Structure> {
        let t = [Some(table)];
        // A sole INTEGER PRIMARY KEY is the rowid, which numbers itself.
        let pk_count = self
            .fetch_with("select count(*) from pragma_table_info($1) where pk > 0", &t)
            .await?
            .into_iter()
            .next()
            .and_then(|mut r| take(&mut r, 0))
            .unwrap_or_default();
        let columns = self
            .fetch_with("select name, type, \"notnull\", dflt_value, pk from pragma_table_info($1) order by cid", &t)
            .await?
            .into_iter()
            .map(|mut r| {
                let data_type = take(&mut r, 1).unwrap_or_default();
                let primary = take(&mut r, 4).is_some_and(|pk| pk != "0");
                ColumnInfo {
                    name: take(&mut r, 0).unwrap_or_default(),
                    auto_increment: primary && pk_count == "1" && data_type.eq_ignore_ascii_case("integer"),
                    data_type,
                    nullable: take(&mut r, 2).as_deref() == Some("0"),
                    default: take(&mut r, 3),
                    primary,
                    ..ColumnInfo::default()
                }
            })
            .collect();
        let mut indexes = Vec::new();
        let list_rows =
            self.fetch_with("select name, \"unique\", origin from pragma_index_list($1) order by name", &t).await?;
        for mut r in list_rows {
            let name = take(&mut r, 0).unwrap_or_default();
            let cols = self
                .fetch_with(
                    "select coalesce(name, '<expression>') from pragma_index_info($1) order by seqno",
                    &[Some(name.as_str())],
                )
                .await?
                .into_iter()
                .filter_map(|mut c| take(&mut c, 0))
                .collect();
            let primary = take(&mut r, 2).as_deref() == Some("pk");
            indexes.push(IndexInfo {
                unique: take(&mut r, 1).as_deref() == Some("1"),
                primary,
                name,
                columns: cols,
                method: None,
                constraint: primary,
            });
        }
        let mut foreign_keys: Vec<ForeignKeyInfo> = Vec::new();
        let fk_rows = self
            .fetch_with(
                "select id, \"table\", \"from\", \"to\", on_update, on_delete from pragma_foreign_key_list($1) \
                 order by id, seq",
                &t,
            )
            .await?;
        for mut r in fk_rows {
            // SQLite foreign keys have no names; the id numbers them.
            let name = format!("fk_{}", take(&mut r, 0).unwrap_or_default());
            let (ref_table, from, to) = (take(&mut r, 1), take(&mut r, 2), take(&mut r, 3));
            match foreign_keys.last_mut() {
                Some(fk) if fk.name == name => {
                    fk.columns.extend(from);
                    fk.ref_columns.extend(to);
                }
                _ => foreign_keys.push(ForeignKeyInfo {
                    name,
                    columns: from.into_iter().collect(),
                    ref_table: ref_table.unwrap_or_default(),
                    ref_columns: to.into_iter().collect(),
                    on_update: take(&mut r, 4).unwrap_or_else(|| "NO ACTION".into()),
                    on_delete: take(&mut r, 5).unwrap_or_else(|| "NO ACTION".into()),
                }),
            }
        }
        let triggers = self
            .fetch_with(
                "select name, sql from sqlite_master where type = 'trigger' and tbl_name = $1 order by name",
                &t,
            )
            .await?
            .into_iter()
            .map(|mut r| lite_trigger(take(&mut r, 0).unwrap_or_default(), take(&mut r, 1).unwrap_or_default()))
            .collect();
        // SQLite keeps checks only in the CREATE statement; the rebuild reads them back the same way to drop one.
        let create = self.fetch_with("select sql from sqlite_master where type = 'table' and name = $1", &t).await?;
        let checks = create
            .into_iter()
            .next()
            .and_then(|mut r| take(&mut r, 0))
            .map(|sql| crate::ddl::lite_checks(&sql))
            .unwrap_or_default();
        Ok(Structure { columns, indexes, foreign_keys, checks, triggers, comment: None, options: None })
    }

    async fn named(&mut self, sql: &str, params: &[Option<&str>]) -> Result<Vec<NamedDef>> {
        Ok(self
            .fetch_with(sql, params)
            .await?
            .into_iter()
            .map(|mut r| NamedDef {
                name: take(&mut r, 0).unwrap_or_default(),
                definition: take(&mut r, 1).unwrap_or_default(),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::lite_trigger;
    use crate::db::{Conn, Engine, Options};
    use crate::tls::Tls;

    #[test]
    fn lite_trigger_should_read_timing_event_and_body_of_a_plain_row_trigger() {
        let t = lite_trigger(
            "t".into(),
            "CREATE TRIGGER \"after insert\" AFTER DELETE ON pets FOR EACH ROW BEGIN\n  select 1;\nEND".into(),
        );
        assert_eq!(
            (t.timing.as_deref(), t.events, t.body.as_deref()),
            (Some("AFTER"), vec!["DELETE".to_string()], Some("select 1;"))
        );
    }

    #[test]
    fn lite_trigger_should_default_to_before_like_sqlite() {
        let t = lite_trigger("t".into(), "create trigger t insert on pets begin select 1; end".into());
        assert_eq!((t.timing.as_deref(), t.events), (Some("BEFORE"), vec!["INSERT".to_string()]));
    }

    #[test]
    fn lite_trigger_should_leave_when_clauses_and_column_lists_to_the_definition() {
        let when = lite_trigger(
            "t".into(),
            "create trigger t after insert on pets when new.id > 1 begin select 1; end".into(),
        );
        let of = lite_trigger("t".into(), "create trigger t after update of name on pets begin select 1; end".into());
        assert!(when.timing.is_none() && when.body.is_none() && of.timing.is_none());
        assert!(of.definition.starts_with("create trigger"));
    }

    async fn sqlite() -> Conn {
        Conn::open(Engine::Sqlite, ":memory:", None, &Tls::default(), &Options::default(), None).await.unwrap()
    }

    #[test]
    fn structure_should_read_sqlite_columns_indexes_keys_and_triggers() {
        tauri::async_runtime::block_on(async {
            let mut c = sqlite().await;
            c.query("create table owners(id integer primary key, name text)").await.unwrap();
            c.query(
                "create table pets(id integer primary key, owner_id integer not null references owners(id) \
                 on delete cascade, name text default 'Rex', born date)",
            )
            .await
            .unwrap();
            c.query("create unique index pets_name on pets(name, born)").await.unwrap();
            c.query("create trigger pets_touch after update on pets begin select 1; end").await.unwrap();
            let s = c.structure("pets").await.unwrap();

            let id = &s.columns[0];
            assert!(id.primary && id.auto_increment && id.nullable, "INTEGER PRIMARY KEY is the rowid");
            let owner = &s.columns[1];
            assert_eq!((owner.name.as_str(), owner.data_type.as_str(), owner.nullable), ("owner_id", "INTEGER", false));
            assert_eq!(s.columns[2].default.as_deref(), Some("'Rex'"));

            let idx = s.indexes.iter().find(|i| i.name == "pets_name").unwrap();
            assert_eq!((idx.columns.clone(), idx.unique), (vec!["name".to_string(), "born".to_string()], true));

            let fk = &s.foreign_keys[0];
            assert_eq!((fk.columns.clone(), fk.ref_table.as_str()), (vec!["owner_id".to_string()], "owners"));
            assert_eq!((fk.on_delete.as_str(), fk.on_update.as_str()), ("CASCADE", "NO ACTION"));

            assert_eq!(s.triggers[0].name, "pets_touch");
        });
    }

    #[test]
    fn structure_should_refuse_a_missing_table() {
        tauri::async_runtime::block_on(async {
            let mut c = sqlite().await;
            assert!(c.structure("nope").await.is_err());
        });
    }
}
