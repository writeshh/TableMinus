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
        // `VIRTUAL GENERATED` / `STORED GENERATED`; not `DEFAULT_GENERATED`, which only marks an expression default.
        generated: extra.split_whitespace().any(|w| w.eq_ignore_ascii_case("GENERATED")),
        data_type,
    }
}

/// The columns of a `DuckDB` `CREATE INDEX name ON table(a, "b")`, read back from its statement: what's between the
/// first parenthesis and the last, split at the commas outside parentheses and quotes, with plain quoted names bare.
fn index_columns(sql: &str) -> Vec<String> {
    let (Some(open), Some(close)) = (sql.find('('), sql.rfind(')')) else { return vec![] };
    let (mut out, mut depth, mut quoted, mut start) = (vec![], 0, false, open + 1);
    for (i, c) in sql[open + 1..close].char_indices().map(|(i, c)| (i + open + 1, c)) {
        match c {
            '"' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth -= 1,
            ',' if !quoted && depth == 0 => {
                out.push(&sql[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&sql[start..close]);
    out.into_iter()
        .map(|c| {
            let c = c.trim();
            let bare = c.strip_prefix('"').and_then(|c| c.strip_suffix('"')).filter(|c| !c.contains('"'));
            bare.unwrap_or(c).to_string()
        })
        .collect()
}

/// A comma-separated list of columns or expressions, split at the commas outside parentheses and quotes.
fn split_list(list: &str) -> Vec<String> {
    let (mut out, mut depth, mut quote, mut start) = (vec![], 0, None, 0);
    for (i, c) in list.char_indices() {
        match (c, quote) {
            ('\'' | '"' | '`', None) => quote = Some(c),
            (q, Some(open)) if q == open => quote = None,
            ('(', None) => depth += 1,
            (')', None) => depth -= 1,
            (',', None) if depth == 0 => {
                out.push(list[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(list[start..].trim().to_string());
    out.retain(|e| !e.is_empty());
    out
}

/// What an Oracle row trigger runs: the PL/SQL after `FOR EACH ROW`, without the `BEGIN … END;` around a plain
/// block, so the structure editor shows and restates it as `trigger_sql` writes it.
fn ora_trigger_body(definition: &str) -> Option<String> {
    // ASCII upper-casing keeps byte offsets, so positions found in `upper` hold in `definition`.
    let upper = definition.to_ascii_uppercase();
    let start = upper.find("FOR EACH ROW")? + "FOR EACH ROW".len();
    let end = upper.find("ALTER TRIGGER").unwrap_or(upper.len());
    let block = definition.get(start..end)?.trim().trim_end_matches('/').trim_end();
    let plain = block.get(..5).is_some_and(|b| b.eq_ignore_ascii_case("begin"));
    let inner = plain.then(|| block.get(5..block.to_ascii_uppercase().rfind("END")?).map(str::trim)).flatten();
    Some(inner.unwrap_or(block).to_string())
}

/// What a SQL Server trigger runs, as `trigger_sql` writes it back: the statements after the `AS` that follows
/// `ON table`, without a `BEGIN … END` around them or the `SET NOCOUNT ON;` that opens it.
fn mssql_trigger_body(definition: &str) -> Option<String> {
    // ASCII upper-casing keeps byte offsets, so positions found in `upper` hold in `definition`.
    let upper = definition.to_ascii_uppercase();
    let on = upper.find(" ON ")?;
    let b = upper.as_bytes();
    let bare = |i: usize| b.get(i).is_none_or(u8::is_ascii_whitespace);
    // The first AS standing alone, outside [quoted] and "quoted" names.
    let mut quote = None;
    let mut at = None;
    for i in on..b.len() {
        match (quote, b[i]) {
            (None, b'[') => quote = Some(b']'),
            (None, b'"') => quote = Some(b'"'),
            (Some(end), c) if c == end => quote = None,
            (None, b'A') if b.get(i + 1) == Some(&b'S') && bare(i.wrapping_sub(1)) && bare(i + 2) => {
                at = Some(i);
                break;
            }
            _ => {}
        }
    }
    let at = at?;
    let mut body = definition.get(at + 2..)?.trim();
    let wrapped = body.get(..5).is_some_and(|b| b.eq_ignore_ascii_case("begin"))
        && body.get(body.len().saturating_sub(3)..).is_some_and(|e| e.eq_ignore_ascii_case("end"));
    if wrapped {
        body = body.get(5..body.len() - 3)?.trim();
    }
    let nocount = "SET NOCOUNT ON;";
    if body.get(..nocount.len()).is_some_and(|s| s.eq_ignore_ascii_case(nocount)) {
        body = body.get(nocount.len()..)?.trim();
    }
    Some(body.to_string())
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
            Dialect::Duck => self.duck_structure(table).await?,
            Dialect::Mssql => self.mssql_structure(table).await?,
            Dialect::Click => self.click_structure(table).await?,
            Dialect::Ora => self.ora_structure(table).await?,
            Dialect::Bq => crate::driver::bigquery::structure(&self.describe(table).await?),
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

    /// A `ClickHouse` table's structure from `system`. Its keys (primary, order and partition) and data skipping
    /// indexes show as indexes; there are no foreign keys or triggers, and constraints aren't listed.
    async fn click_structure(&mut self, table: &str) -> Result<Structure> {
        let t = [Some(table)];
        let columns = self
            .fetch_with(
                "select name, type, if(startsWith(type, 'Nullable('), 'true', 'false'), nullIf(default_expression, ''), \
                 nullIf(comment, ''), if(is_in_primary_key, 'true', 'false'), default_kind from system.columns \
                 where database = currentDatabase() and table = {p1:String} order by position",
                &t,
            )
            .await?
            .into_iter()
            .map(|mut r| {
                let (name, data_type) = (take(&mut r, 0).unwrap_or_default(), take(&mut r, 1).unwrap_or_default());
                let (nullable, default) = (take(&mut r, 2).as_deref() == Some("true"), take(&mut r, 3));
                let (comment, primary) = (take(&mut r, 4), take(&mut r, 5).as_deref() == Some("true"));
                // A MATERIALIZED or ALIAS column would turn into a plain one if it were restated.
                let generated = take(&mut r, 6).is_some_and(|k| matches!(k.as_str(), "MATERIALIZED" | "ALIAS"));
                ColumnInfo { name, data_type, nullable, default, comment, primary, generated, ..ColumnInfo::default() }
            })
            .collect();
        let facts = self
            .fetch_with(
                "select engine, sorting_key, partition_key, primary_key, comment from system.tables \
                 where database = currentDatabase() and name = {p1:String}",
                &t,
            )
            .await?;
        let mut facts = facts.into_iter().next().unwrap_or_default();
        let (engine, sorting, partition, primary, comment) =
            (take(&mut facts, 0), take(&mut facts, 1), take(&mut facts, 2), take(&mut facts, 3), take(&mut facts, 4));
        let key = |name: &str, expr: Option<String>, primary: bool, method: Option<String>| {
            expr.filter(|e| !e.is_empty()).map(|e| IndexInfo {
                name: name.into(),
                columns: split_list(&e),
                unique: false,
                primary,
                method,
                constraint: true,
            })
        };
        let mut indexes: Vec<IndexInfo> = [
            key("PRIMARY KEY", primary.clone(), true, engine),
            key("ORDER BY", sorting.filter(|s| Some(s) != primary.as_ref()), false, None),
            key("PARTITION BY", partition, false, None),
        ]
        .into_iter()
        .flatten()
        .collect();
        for mut r in self
            .fetch_with(
                "select name, expr, type from system.data_skipping_indices \
                 where database = currentDatabase() and table = {p1:String} order by name",
                &t,
            )
            .await?
        {
            indexes.push(IndexInfo {
                name: take(&mut r, 0).unwrap_or_default(),
                columns: split_list(&take(&mut r, 1).unwrap_or_default()),
                unique: false,
                primary: false,
                method: take(&mut r, 2),
                constraint: false,
            });
        }
        Ok(Structure {
            columns,
            indexes,
            foreign_keys: vec![],
            checks: vec![],
            triggers: vec![],
            comment: comment.filter(|c| !c.is_empty()),
            options: None,
        })
    }

    /// A `SQL Server` table's structure from the `sys` views. Triggers are listed by name and definition only: they
    /// are statement-level (they read `inserted` and `deleted`), so the form for row triggers can't restate them.
    #[expect(clippy::too_many_lines, reason = "one query per kind of thing, each read the same way")]
    async fn mssql_structure(&mut self, table: &str) -> Result<Structure> {
        let name = self.table_ident(table);
        let t = [Some(name.as_str())];
        let columns = self
            .fetch_with(
                "select c.name, type_name(c.user_type_id) + case \
                 when ty.name in ('varchar', 'char', 'varbinary', 'binary') then '(' + case when c.max_length = -1 \
                   then 'max' else cast(c.max_length as varchar(10)) end + ')' \
                 when ty.name in ('nvarchar', 'nchar') then '(' + case when c.max_length = -1 then 'max' \
                   else cast(c.max_length / 2 as varchar(10)) end + ')' \
                 when ty.name in ('decimal', 'numeric') then '(' + cast(c.precision as varchar(10)) + ',' \
                   + cast(c.scale as varchar(10)) + ')' \
                 when ty.name in ('datetime2', 'time', 'datetimeoffset') then '(' + cast(c.scale as varchar(10)) + ')' \
                 else '' end, \
                 case when c.is_nullable = 1 then 'true' else 'false' end, object_definition(c.default_object_id), \
                 cast(ep.value as nvarchar(max)), case when c.is_identity = 1 then 'true' else 'false' end, \
                 case when exists (select 1 from sys.index_columns ic join sys.indexes i on i.object_id = ic.object_id \
                   and i.index_id = ic.index_id where i.is_primary_key = 1 and ic.object_id = c.object_id \
                   and ic.column_id = c.column_id) then 'true' else 'false' end \
                 from sys.columns c join sys.types ty on ty.user_type_id = c.user_type_id \
                 left join sys.extended_properties ep on ep.class = 1 and ep.major_id = c.object_id \
                   and ep.minor_id = c.column_id and ep.name = 'MS_Description' \
                 where c.object_id = object_id(@P1) order by c.column_id",
                &t,
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
                "select i.name, string_agg(case when ic.is_included_column = 0 then c.name end, char(31)) \
                   within group (order by ic.key_ordinal), cast(i.is_unique as varchar(1)), \
                 cast(i.is_primary_key as varchar(1)), i.type_desc, \
                 cast(case when i.is_primary_key = 1 or i.is_unique_constraint = 1 then 1 else 0 end as varchar(1)) \
                 from sys.indexes i join sys.index_columns ic on ic.object_id = i.object_id and ic.index_id = i.index_id \
                 join sys.columns c on c.object_id = ic.object_id and c.column_id = ic.column_id \
                 where i.object_id = object_id(@P1) and i.type > 0 \
                 group by i.name, i.is_unique, i.is_primary_key, i.is_unique_constraint, i.type_desc order by i.name",
                &t,
            )
            .await?
            .into_iter()
            .map(|mut r| IndexInfo {
                name: take(&mut r, 0).unwrap_or_default(),
                columns: list(take(&mut r, 1)),
                unique: take(&mut r, 2).as_deref() == Some("1"),
                primary: take(&mut r, 3).as_deref() == Some("1"),
                method: take(&mut r, 4),
                constraint: take(&mut r, 5).as_deref() == Some("1"),
            })
            .collect();
        let foreign_keys = self
            .fetch_with(
                "select fk.name, string_agg(pc.name, char(31)) within group (order by fkc.constraint_column_id), \
                 case when rs.name = 'dbo' then rt.name else rs.name + '.' + rt.name end, \
                 string_agg(rc.name, char(31)) within group (order by fkc.constraint_column_id), \
                 replace(fk.update_referential_action_desc, '_', ' '), replace(fk.delete_referential_action_desc, '_', ' ') \
                 from sys.foreign_keys fk join sys.foreign_key_columns fkc on fkc.constraint_object_id = fk.object_id \
                 join sys.columns pc on pc.object_id = fkc.parent_object_id and pc.column_id = fkc.parent_column_id \
                 join sys.tables rt on rt.object_id = fk.referenced_object_id \
                 join sys.schemas rs on rs.schema_id = rt.schema_id \
                 join sys.columns rc on rc.object_id = fkc.referenced_object_id and rc.column_id = fkc.referenced_column_id \
                 where fk.parent_object_id = object_id(@P1) \
                 group by fk.name, rs.name, rt.name, fk.update_referential_action_desc, fk.delete_referential_action_desc \
                 order by fk.name",
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
        let checks = self
            .named(
                "select name, definition from sys.check_constraints where parent_object_id = object_id(@P1) order by name",
                &t,
            )
            .await?;
        let triggers = self
            .fetch_with(
                "select tr.name, object_definition(tr.object_id), \
                 case when tr.is_instead_of_trigger = 1 then 'INSTEAD OF' else 'AFTER' end, \
                 (select string_agg(e.type_desc, char(31)) from sys.trigger_events e where e.object_id = tr.object_id), \
                 null from sys.triggers tr where tr.parent_id = object_id(@P1) order by tr.name",
                &t,
            )
            .await?
            .into_iter()
            .map(|mut r| {
                let body = r.get(1).cloned().flatten().and_then(|d| mssql_trigger_body(&d));
                if let Some(cell) = r.get_mut(4) {
                    *cell = body;
                }
                trigger(r)
            })
            .collect();
        let comment = self
            .fetch_with(
                "select cast(value as nvarchar(max)) from sys.extended_properties \
                 where class = 1 and major_id = object_id(@P1) and minor_id = 0 and name = 'MS_Description'",
                &t,
            )
            .await?
            .into_iter()
            .next()
            .and_then(|mut r| take(&mut r, 0));
        Ok(Structure { columns, indexes, foreign_keys, checks, triggers, comment, options: None })
    }

    /// A `DuckDB` table's indexes other than its constraints', with their columns read from the CREATE INDEX text.
    async fn duck_indexes(&mut self, table: &[Option<&str>; 2]) -> Result<Vec<IndexInfo>> {
        let rows = self
            .fetch_with(
                "select index_name, is_unique, sql from duckdb_indexes() where database_name = current_database() \
                 and schema_name = $1 and table_name = $2 order by index_name",
                table,
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|mut r| IndexInfo {
                name: take(&mut r, 0).unwrap_or_default(),
                unique: take(&mut r, 1).as_deref() == Some("true"),
                columns: take(&mut r, 2).map(|sql| index_columns(&sql)).unwrap_or_default(),
                primary: false,
                method: None,
                constraint: false,
            })
            .collect())
    }

    async fn duck_structure(&mut self, table: &str) -> Result<Structure> {
        let (schema, name) = table.split_once('.').unwrap_or(("main", table));
        let t = [Some(schema), Some(name)];
        let constraints = self
            .fetch_with(
                "select constraint_type, constraint_name, constraint_text, \
                 array_to_string(constraint_column_names, chr(31)), referenced_table, \
                 array_to_string(referenced_column_names, chr(31)) from duckdb_constraints() \
                 where database_name = current_database() and schema_name = $1 and table_name = $2 \
                 order by constraint_index",
                &t,
            )
            .await?;
        let mut primary: Vec<String> = Vec::new();
        let mut indexes = Vec::new();
        let mut foreign_keys = Vec::new();
        let mut checks = Vec::new();
        for mut r in constraints {
            let (kind, cname, text) = (take(&mut r, 0), take(&mut r, 1).unwrap_or_default(), take(&mut r, 2));
            let columns = list(take(&mut r, 3));
            match kind.as_deref() {
                Some("PRIMARY KEY") => {
                    primary.clone_from(&columns);
                    indexes.push(IndexInfo {
                        name: cname,
                        columns,
                        unique: true,
                        primary: true,
                        method: None,
                        constraint: true,
                    });
                }
                Some("UNIQUE") => {
                    indexes.push(IndexInfo {
                        name: cname,
                        columns,
                        unique: true,
                        primary: false,
                        method: None,
                        constraint: true,
                    });
                }
                Some("FOREIGN KEY") => foreign_keys.push(ForeignKeyInfo {
                    name: cname,
                    columns,
                    ref_table: take(&mut r, 4).unwrap_or_default(),
                    ref_columns: list(take(&mut r, 5)),
                    // DuckDB has no referential actions.
                    on_update: "NO ACTION".into(),
                    on_delete: "NO ACTION".into(),
                }),
                Some("CHECK") => checks.push(NamedDef { name: cname, definition: text.unwrap_or_default() }),
                _ => {}
            }
        }
        indexes.extend(self.duck_indexes(&t).await?);
        let columns = self
            .fetch_with(
                "select column_name, data_type, is_nullable, column_default, comment from duckdb_columns() \
                 where database_name = current_database() and schema_name = $1 and table_name = $2 \
                 order by column_index",
                &t,
            )
            .await?
            .into_iter()
            .map(|mut r| {
                let name = take(&mut r, 0).unwrap_or_default();
                ColumnInfo {
                    primary: primary.contains(&name),
                    name,
                    data_type: take(&mut r, 1).unwrap_or_default(),
                    nullable: take(&mut r, 2).as_deref() == Some("true"),
                    default: take(&mut r, 3),
                    comment: take(&mut r, 4),
                    ..ColumnInfo::default()
                }
            })
            .collect();
        let comment = self
            .fetch_with(
                "select comment from (select comment, schema_name, table_name from duckdb_tables() \
                 union all select comment, schema_name, view_name from duckdb_views()) \
                 where schema_name = $1 and table_name = $2",
                &t,
            )
            .await?
            .into_iter()
            .next()
            .and_then(|mut r| take(&mut r, 0));
        Ok(Structure { columns, indexes, foreign_keys, checks, triggers: vec![], comment, options: None })
    }

    /// Oracle, in the current schema. Defaults and check conditions come from the `_vc` columns (12.2 and later):
    /// the originals are LONGs.
    #[expect(clippy::too_many_lines, reason = "one catalog query per part of the structure")]
    async fn ora_structure(&mut self, table: &str) -> Result<Structure> {
        let t = [Some(table)];
        let columns = self
            .fetch_with(
                "select c.column_name, c.data_type || case \
                 when c.data_type in ('VARCHAR2', 'NVARCHAR2', 'CHAR', 'NCHAR') then '(' || c.char_length || ')' \
                 when c.data_type = 'RAW' then '(' || c.data_length || ')' \
                 when c.data_type = 'NUMBER' and c.data_precision is not null \
                   then '(' || c.data_precision || ',' || c.data_scale || ')' end, \
                 case when c.nullable = 'Y' then 'true' else 'false' end, \
                 case when c.identity_column = 'NO' then trim(c.data_default_vc) end, m.comments, \
                 case when c.identity_column = 'YES' then 'true' else 'false' end, \
                 case when exists (select 1 from all_constraints k join all_cons_columns kc \
                   on kc.owner = k.owner and kc.constraint_name = k.constraint_name \
                   where k.constraint_type = 'P' and k.owner = c.owner and k.table_name = c.table_name \
                   and kc.column_name = c.column_name) then 'true' else 'false' end \
                 from all_tab_columns c left join all_col_comments m on m.owner = c.owner \
                   and m.table_name = c.table_name and m.column_name = c.column_name \
                 where c.owner = sys_context('USERENV', 'CURRENT_SCHEMA') and c.table_name = :1 order by c.column_id",
                &t,
            )
            .await?
            .into_iter()
            .map(|mut r| ColumnInfo {
                name: take(&mut r, 0).unwrap_or_default(),
                data_type: take(&mut r, 1).unwrap_or_default(),
                nullable: take(&mut r, 2).as_deref() == Some("true"),
                default: take(&mut r, 3).filter(|d| !d.is_empty()),
                comment: take(&mut r, 4),
                auto_increment: take(&mut r, 5).as_deref() == Some("true"),
                primary: take(&mut r, 6).as_deref() == Some("true"),
                ..ColumnInfo::default()
            })
            .collect();
        let indexes = self
            .fetch_with(
                "select i.index_name, (select listagg(ic.column_name, chr(31)) within group (order by ic.column_position) \
                   from all_ind_columns ic where ic.index_owner = i.owner and ic.index_name = i.index_name), \
                 case when i.uniqueness = 'UNIQUE' then '1' else '0' end, \
                 case when exists (select 1 from all_constraints k where k.owner = i.table_owner \
                   and k.index_name = i.index_name and k.constraint_type = 'P') then '1' else '0' end, i.index_type, \
                 case when exists (select 1 from all_constraints k where k.owner = i.table_owner \
                   and k.index_name = i.index_name and k.constraint_type in ('P', 'U')) then '1' else '0' end \
                 from all_indexes i where i.table_owner = sys_context('USERENV', 'CURRENT_SCHEMA') \
                 and i.table_name = :1 and i.index_type <> 'LOB' order by i.index_name",
                &t,
            )
            .await?
            .into_iter()
            .map(|mut r| IndexInfo {
                name: take(&mut r, 0).unwrap_or_default(),
                columns: list(take(&mut r, 1)),
                unique: take(&mut r, 2).as_deref() == Some("1"),
                primary: take(&mut r, 3).as_deref() == Some("1"),
                method: take(&mut r, 4),
                constraint: take(&mut r, 5).as_deref() == Some("1"),
            })
            .collect();
        let foreign_keys = self
            .fetch_with(
                "select k.constraint_name, (select listagg(kc.column_name, chr(31)) within group (order by kc.position) \
                   from all_cons_columns kc where kc.owner = k.owner and kc.constraint_name = k.constraint_name), \
                 r.table_name, (select listagg(rc.column_name, chr(31)) within group (order by rc.position) \
                   from all_cons_columns rc where rc.owner = r.owner and rc.constraint_name = r.constraint_name), \
                 k.delete_rule \
                 from all_constraints k join all_constraints r on r.owner = k.r_owner \
                   and r.constraint_name = k.r_constraint_name \
                 where k.constraint_type = 'R' and k.owner = sys_context('USERENV', 'CURRENT_SCHEMA') \
                 and k.table_name = :1 order by k.constraint_name",
                &t,
            )
            .await?
            .into_iter()
            .map(|mut r| ForeignKeyInfo {
                name: take(&mut r, 0).unwrap_or_default(),
                columns: list(take(&mut r, 1)),
                ref_table: take(&mut r, 2).unwrap_or_default(),
                ref_columns: list(take(&mut r, 3)),
                // Oracle has no ON UPDATE actions.
                on_update: "NO ACTION".into(),
                on_delete: take(&mut r, 4).unwrap_or_else(|| "NO ACTION".into()),
            })
            .collect();
        // NOT NULL is a check constraint too; the column's flag shows it.
        let checks = self
            .named(
                "select constraint_name, search_condition_vc from all_constraints \
                 where owner = sys_context('USERENV', 'CURRENT_SCHEMA') and table_name = :1 and constraint_type = 'C' \
                 and not (generated = 'GENERATED NAME' and search_condition_vc like '\"%\" IS NOT NULL') \
                 order by constraint_name",
                &t,
            )
            .await?;
        let triggers = self
            .fetch_with(
                "select trigger_name, dbms_metadata.get_ddl('TRIGGER', trigger_name, owner), \
                 case when trigger_type in ('BEFORE EACH ROW', 'AFTER EACH ROW') and when_clause is null \
                   then substr(trigger_type, 1, instr(trigger_type, ' ') - 1) end, \
                 replace(triggering_event, ' OR ', chr(31)), null \
                 from all_triggers where table_owner = sys_context('USERENV', 'CURRENT_SCHEMA') \
                 and table_name = :1 and base_object_type = 'TABLE' order by trigger_name",
                &t,
            )
            .await?
            .into_iter()
            .map(|mut r| {
                // The body is a LONG; it's read back from the definition, between the row clause and the end.
                let body = r.get(1).cloned().flatten().and_then(|d| ora_trigger_body(&d));
                if let Some(cell) = r.get_mut(4) {
                    *cell = body;
                }
                trigger(r)
            })
            .collect();
        let comment = self
            .fetch_with(
                "select comments from all_tab_comments where owner = sys_context('USERENV', 'CURRENT_SCHEMA') \
                 and table_name = :1",
                &t,
            )
            .await?
            .into_iter()
            .next()
            .and_then(|mut r| take(&mut r, 0));
        Ok(Structure { columns, indexes, foreign_keys, checks, triggers, comment, options: None })
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
    use super::{lite_trigger, mssql_trigger_body, my_column};
    use crate::db::{Conn, Engine, Options};
    use crate::tls::Tls;

    #[test]
    fn mssql_trigger_body_should_read_back_what_trigger_sql_wrote() {
        let def = "CREATE TRIGGER [app].[stamp] ON [app].[pets] AFTER INSERT, UPDATE AS\nBEGIN\n  SET NOCOUNT ON;\n  UPDATE x SET y = 1\nEND";
        assert_eq!(mssql_trigger_body(def).as_deref(), Some("UPDATE x SET y = 1"));
        let plain = "create trigger t on [has as in it] for delete as delete from log";
        assert_eq!(mssql_trigger_body(plain).as_deref(), Some("delete from log"));
    }

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

    fn my_extra(extra: &str) -> Vec<Option<String>> {
        ["c", "timestamp", "YES", "CURRENT_TIMESTAMP", "", extra, "", ""].map(|v| Some(v.to_string())).into()
    }

    #[test]
    fn my_column_should_not_take_an_expression_default_for_a_generated_column() {
        assert!(!my_column(false, my_extra("DEFAULT_GENERATED on update CURRENT_TIMESTAMP")).generated);
        assert!(my_column(false, my_extra("VIRTUAL GENERATED")).generated);
        assert!(my_column(true, my_extra("STORED GENERATED")).generated);
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
