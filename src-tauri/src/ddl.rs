//! DDL from typed requests: structure edits (columns, indexes, foreign keys, checks, triggers, the primary key, name,
//! comment, MySQL's engine and collation) and table operations
//! (clone, truncate, drop, reset auto-increment, schemas). Statements are generated first, so the UI can show
//! them, then run as shown by [`Conn::run_ddl`].
//!
//! DDL takes no bound parameters: identifiers go through [`Dialect::ident`] and the few string values
//! (comments, names inside catalog functions) through [`Dialect::literal`]. Types and defaults are SQL the user
//! typed into the structure editor, inserted as written, like SQL in the editor.

use crate::db::{Conn, Dialect};
use crate::error::{Error, Result};
use crate::structure::{ColumnInfo, Structure};
use serde::Deserialize;

/// A foreign key's ON UPDATE / ON DELETE action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Rule {
    #[serde(rename = "NO ACTION")]
    NoAction,
    #[serde(rename = "RESTRICT")]
    Restrict,
    #[serde(rename = "CASCADE")]
    Cascade,
    #[serde(rename = "SET NULL")]
    SetNull,
    #[serde(rename = "SET DEFAULT")]
    SetDefault,
}

impl Rule {
    fn sql(self) -> &'static str {
        match self {
            Rule::NoAction => "NO ACTION",
            Rule::Restrict => "RESTRICT",
            Rule::Cascade => "CASCADE",
            Rule::SetNull => "SET NULL",
            Rule::SetDefault => "SET DEFAULT",
        }
    }
}

/// An index's access method, where the engine has a choice: Postgres has them all, MySQL `BTREE` and `HASH`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexMethod {
    Btree,
    Hash,
    Gist,
    Gin,
    Brin,
    Spgist,
}

impl IndexMethod {
    fn sql(self, d: Dialect) -> Result<&'static str> {
        match (d, self) {
            (Dialect::Pg, IndexMethod::Btree) => Ok("btree"),
            (Dialect::Pg, IndexMethod::Hash) => Ok("hash"),
            (Dialect::Pg, IndexMethod::Gist) => Ok("gist"),
            (Dialect::Pg, IndexMethod::Gin) => Ok("gin"),
            (Dialect::Pg, IndexMethod::Brin) => Ok("brin"),
            (Dialect::Pg, IndexMethod::Spgist) => Ok("spgist"),
            (Dialect::My, IndexMethod::Btree) => Ok("BTREE"),
            (Dialect::My, IndexMethod::Hash) => Ok("HASH"),
            (Dialect::My, _) => Err(Error::Invalid("MySQL indexes are BTREE or HASH")),
            (Dialect::Lite, _) => Err(Error::Invalid("SQLite has only one kind of index")),
        }
    }
}

/// When a row trigger fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum TriggerTiming {
    #[serde(rename = "BEFORE")]
    Before,
    #[serde(rename = "AFTER")]
    After,
}

/// What a row trigger fires on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum TriggerEvent {
    #[serde(rename = "INSERT")]
    Insert,
    #[serde(rename = "UPDATE")]
    Update,
    #[serde(rename = "DELETE")]
    Delete,
}

impl TriggerEvent {
    fn sql(self) -> &'static str {
        match self {
            TriggerEvent::Insert => "INSERT",
            TriggerEvent::Update => "UPDATE",
            TriggerEvent::Delete => "DELETE",
        }
    }
}

/// A column as the structure editor wants it. Blank defaults and comments mean none.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColumnDef {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub comment: Option<String>,
    pub auto_increment: bool,
    /// Part of the primary key. Only settable when creating a table.
    pub primary: bool,
}

/// One staged structure edit.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Change {
    AddColumn {
        column: ColumnDef,
    },
    DropColumn {
        name: String,
    },
    /// `from` is the column's current name; `column` is all of it as wanted.
    ModifyColumn {
        from: String,
        column: ColumnDef,
    },
    AddIndex {
        name: String,
        columns: Vec<String>,
        unique: bool,
        /// None: the engine's default (btree).
        #[serde(default)]
        method: Option<IndexMethod>,
    },
    DropIndex {
        name: String,
    },
    AddForeignKey {
        name: String,
        columns: Vec<String>,
        /// `schema.table` on Postgres, like every table name.
        ref_table: String,
        ref_columns: Vec<String>,
        on_update: Rule,
        on_delete: Rule,
    },
    DropForeignKey {
        name: String,
    },
    /// `expression` is SQL the user typed, inserted as written, like a default.
    AddCheck {
        name: String,
        expression: String,
    },
    DropCheck {
        name: String,
    },
    /// A row trigger. `body` is SQL the user typed: MySQL's and SQLite's statements (SQLite's go inside BEGIN … END),
    /// or the function Postgres calls, like `audit()`.
    AddTrigger {
        name: String,
        timing: TriggerTiming,
        events: Vec<TriggerEvent>,
        body: String,
    },
    DropTrigger {
        name: String,
    },
    /// Replaces an existing table's primary key with one on `columns`, in order; none when empty.
    SetPrimaryKey {
        columns: Vec<String>,
    },
    /// MySQL's storage engine (`InnoDB`…).
    Engine {
        engine: String,
    },
    /// MySQL's default collation for the table's new columns.
    Collation {
        collation: String,
    },
    Rename {
        to: String,
    },
    Comment {
        comment: Option<String>,
    },
}

/// A table operation from the sidebar menu. Options another engine has no use for are ignored there.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum TableOp {
    Rename {
        table: String,
        to: String,
    },
    Clone {
        table: String,
        to: String,
        data: bool,
    },
    /// `cascade` and `restart_identity` are Postgres's; `ignore_fk` is MySQL's.
    Truncate {
        table: String,
        cascade: bool,
        restart_identity: bool,
        ignore_fk: bool,
    },
    /// `cascade` is Postgres's; `ignore_fk` is MySQL's.
    Drop {
        table: String,
        cascade: bool,
        ignore_fk: bool,
    },
    ResetAutoIncrement {
        table: String,
    },
    CreateSchema {
        name: String,
    },
    RenameSchema {
        name: String,
        to: String,
    },
    DropSchema {
        name: String,
        cascade: bool,
    },
}

/// A database operation from the database window (Postgres and MySQL; a SQLite file is its one database).
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum DbOp {
    /// A blank `encoding` or `collation` keeps the server's default. MySQL's encoding is its character set.
    Create {
        name: String,
        encoding: Option<String>,
        collation: Option<String>,
    },
    Rename {
        name: String,
        to: String,
    },
    Drop {
        name: String,
    },
}

/// What the new database form offers: the server's encodings, and its collations as (collation, encoding) pairs.
/// Postgres's collations are locales, which go with any encoding (the encoding is blank).
#[derive(Debug, serde::Serialize)]
pub struct DbOptions {
    pub encodings: Vec<String>,
    pub collations: Vec<(String, String)>,
}

fn create_database_sql(d: Dialect, name: &str, encoding: Option<&str>, collation: Option<&str>) -> Result<String> {
    let name = d.ident(not_blank(name, "the database needs a name")?);
    let clause = |keyword: &str, v: Option<&str>| v.map(|v| format!(" {keyword} {}", d.literal(v))).unwrap_or_default();
    let options = if d == Dialect::Pg {
        // Only template0 may be copied with an encoding or locale other than its own.
        let template = if encoding.is_some() || collation.is_some() { " TEMPLATE template0" } else { "" };
        format!("{}{}{template}", clause("ENCODING", encoding), clause("LC_COLLATE", collation))
    } else {
        format!("{}{}", clause("CHARACTER SET", encoding), clause("COLLATE", collation))
    };
    Ok(format!("CREATE DATABASE {name}{options}"))
}

fn blank_is_none(s: Option<&String>) -> Option<&str> {
    s.map(|s| s.trim()).filter(|s| !s.is_empty())
}

fn not_blank<'s>(s: &'s str, what: &'static str) -> Result<&'s str> {
    let s = s.trim();
    if s.is_empty() { Err(Error::Invalid(what)) } else { Ok(s) }
}

fn idents(d: Dialect, names: &[String]) -> String {
    names.iter().map(|n| d.ident(n)).collect::<Vec<_>>().join(", ")
}

/// A column definition for CREATE TABLE, ADD COLUMN or MySQL's CHANGE COLUMN.
fn column_sql(d: Dialect, c: &ColumnDef, on_update: Option<&str>) -> Result<String> {
    let mut s = format!(
        "{} {}",
        d.ident(not_blank(&c.name, "every column needs a name")?),
        not_blank(&c.data_type, "every column needs a type")?
    );
    if c.auto_increment && d == Dialect::Pg {
        s.push_str(" GENERATED BY DEFAULT AS IDENTITY");
    }
    s.push_str(if c.nullable { " NULL" } else { " NOT NULL" });
    if let Some(default) = blank_is_none(c.default.as_ref()) {
        s.push_str(" DEFAULT ");
        s.push_str(default);
    }
    if d == Dialect::My {
        if let Some(on_update) = on_update {
            s.push_str(" ON UPDATE ");
            s.push_str(on_update);
        }
        if c.auto_increment {
            s.push_str(" AUTO_INCREMENT");
        }
        if let Some(comment) = blank_is_none(c.comment.as_ref()) {
            s.push_str(" COMMENT ");
            s.push_str(&d.literal(comment));
        }
    }
    Ok(s)
}

fn index_sql(
    d: Dialect,
    t: &str,
    name: &str,
    columns: &[String],
    unique: bool,
    method: Option<IndexMethod>,
) -> Result<String> {
    if columns.is_empty() {
        return Err(Error::Invalid("an index needs at least one column"));
    }
    let unique = if unique { "UNIQUE " } else { "" };
    let name = d.ident(not_blank(name, "every index needs a name")?);
    let cols = idents(d, columns);
    Ok(match (d, method.map(|m| m.sql(d)).transpose()?) {
        (Dialect::Pg, Some(m)) => format!("CREATE {unique}INDEX {name} ON {t} USING {m} ({cols})"),
        (Dialect::My, Some(m)) => format!("CREATE {unique}INDEX {name} ON {t} ({cols}) USING {m}"),
        _ => format!("CREATE {unique}INDEX {name} ON {t} ({cols})"),
    })
}

fn check_sql(d: Dialect, name: &str, expression: &str) -> Result<String> {
    let expression = not_blank(expression, "a check needs an expression")?;
    let named = if name.trim().is_empty() { String::new() } else { format!("CONSTRAINT {} ", d.ident(name.trim())) };
    Ok(format!("{named}CHECK ({expression})"))
}

fn trigger_sql(
    d: Dialect,
    t: &str,
    name: &str,
    timing: TriggerTiming,
    events: &[TriggerEvent],
    body: &str,
) -> Result<String> {
    let name = d.ident(not_blank(name, "every trigger needs a name")?);
    let body = not_blank(body, "a trigger needs something to run")?;
    let timing = match timing {
        TriggerTiming::Before => "BEFORE",
        TriggerTiming::After => "AFTER",
    };
    let one = match events {
        [] => return Err(Error::Invalid("a trigger needs an event to fire on")),
        [e] => Some(e.sql()),
        _ => None,
    };
    Ok(match d {
        Dialect::Pg => {
            let events = events.iter().map(|e| e.sql()).collect::<Vec<_>>().join(" OR ");
            format!("CREATE TRIGGER {name} {timing} {events} ON {t} FOR EACH ROW EXECUTE FUNCTION {body}")
        }
        Dialect::My => {
            let event = one.ok_or(Error::Invalid("MySQL and SQLite triggers fire on one event"))?;
            format!("CREATE TRIGGER {name} {timing} {event} ON {t} FOR EACH ROW {body}")
        }
        Dialect::Lite => {
            let event = one.ok_or(Error::Invalid("MySQL and SQLite triggers fire on one event"))?;
            let end = if body.ends_with(';') { "" } else { ";" };
            format!("CREATE TRIGGER {name} {timing} {event} ON {t} FOR EACH ROW BEGIN\n  {body}{end}\nEND")
        }
    })
}

fn drop_trigger_sql(d: Dialect, t: &str, name: &str) -> String {
    match d {
        Dialect::Pg => format!("DROP TRIGGER {} ON {t}", d.ident(name)),
        _ => format!("DROP TRIGGER {}", d.ident(name)),
    }
}

/// The statements that put the primary key on `columns` (none when empty) in place of the current one.
fn primary_key_sql(d: Dialect, t: &str, current: &Structure, columns: &[String]) -> Result<Vec<String>> {
    let old = current.indexes.iter().find(|i| i.primary);
    let add = (!columns.is_empty()).then(|| format!("ADD PRIMARY KEY ({})", idents(d, columns)));
    Ok(match d {
        Dialect::Lite => return Err(Error::SqliteRebuild("change the primary key")),
        // One statement, so MySQL (which commits DDL as it goes) never leaves the table without its key.
        Dialect::My => {
            let parts: Vec<String> = old.map(|_| "DROP PRIMARY KEY".to_string()).into_iter().chain(add).collect();
            if parts.is_empty() { vec![] } else { vec![format!("ALTER TABLE {t} {}", parts.join(", "))] }
        }
        Dialect::Pg => old
            .map(|i| format!("ALTER TABLE {t} DROP CONSTRAINT {}", d.ident(&i.name)))
            .into_iter()
            .chain(add.map(|a| format!("ALTER TABLE {t} {a}")))
            .collect(),
    })
}

/// A MySQL table option's value (an engine or a collation). It can't be a bound parameter, so it must be a plain name.
fn mysql_option(d: Dialect, value: &str) -> Result<&str> {
    if d != Dialect::My {
        return Err(Error::Invalid("only MySQL tables have an engine and a collation"));
    }
    let value = value.trim();
    if value.is_empty() || !value.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(Error::Invalid("an engine or collation is a name of letters, digits and _"));
    }
    Ok(value)
}

fn foreign_key_sql(d: Dialect, name: &str, columns: &[String], ref_table: &str, ref_columns: &[String]) -> String {
    let named = if name.trim().is_empty() { String::new() } else { format!("CONSTRAINT {} ", d.ident(name.trim())) };
    format!(
        "{named}FOREIGN KEY ({}) REFERENCES {} ({})",
        idents(d, columns),
        d.table_ident(ref_table),
        idents(d, ref_columns)
    )
}

/// The statements that make `changes` to `table`, whose structure is `current`; with no `current`, the
/// statements that create it. A rename comes last, so everything before it can use the old name.
pub fn alter_sql(d: Dialect, table: &str, current: Option<&Structure>, changes: &[Change]) -> Result<Vec<String>> {
    let t = d.table_ident(not_blank(table, "the table needs a name")?);
    let Some(current) = current else { return create_sql(d, &t, changes) };
    let comment_on_column = |column: &str, comment: Option<&str>| {
        format!(
            "COMMENT ON COLUMN {t}.{} IS {}",
            d.ident(column),
            comment.map_or_else(|| "NULL".into(), |c| d.literal(c))
        )
    };
    let mut out = Vec::new();
    let mut rename = None;
    for change in changes {
        match change {
            Change::AddColumn { column } => {
                if column.primary {
                    return Err(Error::Invalid("changing the primary key of an existing table isn't supported yet"));
                }
                if d == Dialect::Lite && blank_is_none(column.comment.as_ref()).is_some() {
                    return Err(Error::Invalid("SQLite columns have no comments"));
                }
                out.push(format!("ALTER TABLE {t} ADD COLUMN {}", column_sql(d, column, None)?));
                if let (Dialect::Pg, Some(comment)) = (d, blank_is_none(column.comment.as_ref())) {
                    out.push(comment_on_column(column.name.trim(), Some(comment)));
                }
            }
            Change::DropColumn { name } => out.push(format!("ALTER TABLE {t} DROP COLUMN {}", d.ident(name))),
            Change::ModifyColumn { from, column } => {
                let cur = current
                    .columns
                    .iter()
                    .find(|c| &c.name == from)
                    .ok_or(Error::Invalid("a changed column no longer exists; reopen the structure"))?;
                out.extend(modify_column_sql(d, &t, cur, column)?);
            }
            Change::AddIndex { name, columns, unique, method } => {
                out.push(index_sql(d, &t, name, columns, *unique, *method)?);
            }
            Change::DropIndex { name } => {
                let index = current.indexes.iter().find(|i| &i.name == name);
                let constraint = index.is_some_and(|i| i.constraint);
                out.push(match d {
                    Dialect::Pg if constraint => format!("ALTER TABLE {t} DROP CONSTRAINT {}", d.ident(name)),
                    // An index lives in its table's schema.
                    Dialect::Pg => match table.split_once('.') {
                        Some((schema, _)) => format!("DROP INDEX {}.{}", d.ident(schema), d.ident(name)),
                        None => format!("DROP INDEX {}", d.ident(name)),
                    },
                    Dialect::My if index.is_some_and(|i| i.primary) => format!("ALTER TABLE {t} DROP PRIMARY KEY"),
                    Dialect::My => format!("DROP INDEX {} ON {t}", d.ident(name)),
                    Dialect::Lite => format!("DROP INDEX {}", d.ident(name)),
                });
            }
            Change::AddForeignKey { name, columns, ref_table, ref_columns, on_update, on_delete } => {
                if d == Dialect::Lite {
                    return Err(Error::SqliteRebuild("add a foreign key"));
                }
                if columns.is_empty() || columns.len() != ref_columns.len() {
                    return Err(Error::Invalid("a foreign key needs as many referenced columns as columns"));
                }
                out.push(format!(
                    "ALTER TABLE {t} ADD {} ON UPDATE {} ON DELETE {}",
                    foreign_key_sql(d, name, columns, ref_table, ref_columns),
                    on_update.sql(),
                    on_delete.sql()
                ));
            }
            Change::DropForeignKey { name } => out.push(match d {
                Dialect::Pg => format!("ALTER TABLE {t} DROP CONSTRAINT {}", d.ident(name)),
                Dialect::My => format!("ALTER TABLE {t} DROP FOREIGN KEY {}", d.ident(name)),
                Dialect::Lite => return Err(Error::SqliteRebuild("drop a foreign key")),
            }),
            Change::AddCheck { name, expression } => {
                if d == Dialect::Lite {
                    return Err(Error::SqliteRebuild("add a check constraint"));
                }
                out.push(format!("ALTER TABLE {t} ADD {}", check_sql(d, name, expression)?));
            }
            Change::DropCheck { name } => out.push(match d {
                Dialect::Lite => return Err(Error::SqliteRebuild("drop a check constraint")),
                // MariaDB and MySQL 8.0.19+ both read DROP CONSTRAINT; MySQL's own DROP CHECK isn't MariaDB's.
                _ => format!("ALTER TABLE {t} DROP CONSTRAINT {}", d.ident(name)),
            }),
            Change::AddTrigger { name, timing, events, body } => {
                out.push(trigger_sql(d, &t, name, *timing, events, body)?)
            }
            Change::DropTrigger { name } => out.push(drop_trigger_sql(d, &t, name)),
            Change::SetPrimaryKey { columns } => out.extend(primary_key_sql(d, &t, current, columns)?),
            Change::Engine { engine } => out.push(format!("ALTER TABLE {t} ENGINE = {}", mysql_option(d, engine)?)),
            Change::Collation { collation } => {
                out.push(format!("ALTER TABLE {t} COLLATE = {}", mysql_option(d, collation)?));
            }
            Change::Rename { to } => rename = Some(rename_sql(d, &t, not_blank(to, "the table needs a name")?)),
            Change::Comment { comment } => {
                let comment = blank_is_none(comment.as_ref());
                out.push(match d {
                    Dialect::Pg => {
                        format!("COMMENT ON TABLE {t} IS {}", comment.map_or_else(|| "NULL".into(), |c| d.literal(c)))
                    }
                    Dialect::My => format!("ALTER TABLE {t} COMMENT = {}", d.literal(comment.unwrap_or_default())),
                    Dialect::Lite => return Err(Error::Invalid("SQLite tables have no comments")),
                });
            }
        }
    }
    out.extend(rename);
    Ok(out)
}

fn rename_sql(d: Dialect, t: &str, to: &str) -> String {
    // Postgres renames within the schema, so the new name is never schema-qualified.
    let to = if d == Dialect::Pg { to.rsplit_once('.').map_or(to, |(_, name)| name) } else { to };
    match d {
        Dialect::My => format!("RENAME TABLE {t} TO {}", d.ident(to)),
        _ => format!("ALTER TABLE {t} RENAME TO {}", d.ident(to)),
    }
}

fn modify_column_sql(d: Dialect, t: &str, cur: &ColumnInfo, want: &ColumnDef) -> Result<Vec<String>> {
    let name = not_blank(&want.name, "every column needs a name")?;
    let data_type = not_blank(&want.data_type, "every column needs a type")?;
    let default = blank_is_none(want.default.as_ref());
    let comment = blank_is_none(want.comment.as_ref());
    let renamed = name != cur.name;
    let retyped = !data_type.eq_ignore_ascii_case(&cur.data_type);
    let renulled = want.nullable != cur.nullable;
    let redefaulted = default != cur.default.as_deref();
    let recommented = comment != cur.comment.as_deref();
    let reincremented = want.auto_increment != cur.auto_increment;
    if want.primary != cur.primary {
        return Err(Error::Invalid("changing the primary key of an existing table isn't supported yet"));
    }
    let col = d.ident(name);
    let mut out = Vec::new();
    match d {
        Dialect::My => {
            if renamed || retyped || renulled || redefaulted || recommented || reincremented {
                let def = column_sql(d, want, cur.on_update.as_deref())?;
                out.push(format!("ALTER TABLE {t} CHANGE COLUMN {} {def}", d.ident(&cur.name)));
            }
        }
        Dialect::Lite => {
            if retyped || renulled || redefaulted || reincremented {
                return Err(Error::SqliteRebuild("change a column's type, nullability, default or auto-increment"));
            }
            if recommented {
                return Err(Error::Invalid("SQLite columns have no comments"));
            }
            if renamed {
                out.push(format!("ALTER TABLE {t} RENAME COLUMN {} TO {col}", d.ident(&cur.name)));
            }
        }
        Dialect::Pg => {
            let alter = |what: &str| format!("ALTER TABLE {t} ALTER COLUMN {col} {what}");
            if renamed {
                out.push(format!("ALTER TABLE {t} RENAME COLUMN {} TO {col}", d.ident(&cur.name)));
            }
            if retyped {
                // USING converts what an implicit cast won't (text to integer, say).
                out.push(alter(&format!("TYPE {data_type} USING {col}::{data_type}")));
            }
            if renulled {
                out.push(alter(if want.nullable { "DROP NOT NULL" } else { "SET NOT NULL" }));
            }
            if reincremented {
                out.push(alter(if want.auto_increment {
                    "ADD GENERATED BY DEFAULT AS IDENTITY"
                } else {
                    "DROP IDENTITY IF EXISTS"
                }));
            }
            if redefaulted {
                out.push(alter(&default.map_or_else(|| "DROP DEFAULT".into(), |v| format!("SET DEFAULT {v}"))));
            }
            if recommented {
                out.push(format!(
                    "COMMENT ON COLUMN {t}.{col} IS {}",
                    comment.map_or_else(|| "NULL".into(), |c| d.literal(c))
                ));
            }
        }
    }
    Ok(out)
}

/// CREATE TABLE with the added columns, foreign keys and checks inline (MySQL: its engine and collation too), then
/// its indexes and (Postgres) comments.
fn create_sql(d: Dialect, t: &str, changes: &[Change]) -> Result<Vec<String>> {
    let mut defs = Vec::new();
    let mut primary = Vec::new();
    let mut after = Vec::new();
    let mut table_comment = None;
    let mut options = Vec::new();
    for change in changes {
        match change {
            Change::AddColumn { column } => {
                defs.push(column_sql(d, column, None)?);
                if column.primary {
                    primary.push(column.name.trim().to_string());
                }
                match (d, blank_is_none(column.comment.as_ref())) {
                    (Dialect::Pg, Some(c)) => {
                        after.push(format!(
                            "COMMENT ON COLUMN {t}.{} IS {}",
                            d.ident(column.name.trim()),
                            d.literal(c)
                        ));
                    }
                    (Dialect::Lite, Some(_)) => return Err(Error::Invalid("SQLite columns have no comments")),
                    _ => {}
                }
            }
            Change::AddIndex { name, columns, unique, method } => {
                after.push(index_sql(d, t, name, columns, *unique, *method)?);
            }
            Change::AddCheck { name, expression } => defs.push(check_sql(d, name, expression)?),
            Change::AddTrigger { name, timing, events, body } => {
                after.push(trigger_sql(d, t, name, *timing, events, body)?)
            }
            Change::Engine { engine } => options.push(format!(" ENGINE = {}", mysql_option(d, engine)?)),
            Change::Collation { collation } => options.push(format!(" COLLATE = {}", mysql_option(d, collation)?)),
            Change::AddForeignKey { name, columns, ref_table, ref_columns, on_update, on_delete } => {
                if columns.is_empty() || columns.len() != ref_columns.len() {
                    return Err(Error::Invalid("a foreign key needs as many referenced columns as columns"));
                }
                defs.push(format!(
                    "{} ON UPDATE {} ON DELETE {}",
                    foreign_key_sql(d, name, columns, ref_table, ref_columns),
                    on_update.sql(),
                    on_delete.sql()
                ));
            }
            Change::Comment { comment } => table_comment = blank_is_none(comment.as_ref()),
            _ => {
                return Err(Error::Invalid(
                    "a new table can only have columns, indexes, foreign keys, checks and triggers",
                ));
            }
        }
    }
    if !changes.iter().any(|c| matches!(c, Change::AddColumn { .. })) {
        return Err(Error::Invalid("a new table needs at least one column"));
    }
    if !primary.is_empty() {
        // Inserted after the columns, before the foreign keys.
        let n = changes.iter().filter(|c| matches!(c, Change::AddColumn { .. })).count();
        defs.insert(n, format!("PRIMARY KEY ({})", idents(d, &primary)));
    }
    let mut create = format!("CREATE TABLE {t} (\n  {}\n){}", defs.join(",\n  "), options.concat());
    match (d, table_comment) {
        (Dialect::My, Some(c)) => create = format!("{create} COMMENT = {}", d.literal(c)),
        (Dialect::Pg, Some(c)) => after.insert(0, format!("COMMENT ON TABLE {t} IS {}", d.literal(c))),
        (Dialect::Lite, Some(_)) => return Err(Error::Invalid("SQLite tables have no comments")),
        _ => {}
    }
    Ok(std::iter::once(create).chain(after).collect())
}

/// Postgres has no `SHOW CREATE TABLE`, so its DDL is written from the structure.
pub fn pg_create_sql(table: &str, s: &Structure) -> String {
    let d = Dialect::Pg;
    let t = d.table_ident(table);
    let mut defs: Vec<String> = s
        .columns
        .iter()
        .map(|c| {
            let mut def = format!("{} {}", d.ident(&c.name), c.data_type);
            if !c.nullable {
                def.push_str(" NOT NULL");
            }
            if let Some(v) = &c.default {
                def.push_str(" DEFAULT ");
                def.push_str(v);
            }
            def
        })
        .collect();
    for i in s.indexes.iter().filter(|i| i.primary) {
        defs.push(format!("CONSTRAINT {} PRIMARY KEY ({})", d.ident(&i.name), idents(d, &i.columns)));
    }
    for fk in &s.foreign_keys {
        defs.push(format!(
            "{} ON UPDATE {} ON DELETE {}",
            foreign_key_sql(d, &fk.name, &fk.columns, &fk.ref_table, &fk.ref_columns),
            fk.on_update,
            fk.on_delete
        ));
    }
    for c in &s.checks {
        defs.push(format!("CONSTRAINT {} {}", d.ident(&c.name), c.definition));
    }
    let mut out = vec![format!("CREATE TABLE {t} (\n  {}\n);", defs.join(",\n  "))];
    for i in s.indexes.iter().filter(|i| !i.primary) {
        let unique = if i.unique { "UNIQUE " } else { "" };
        let using = i.method.as_deref().filter(|m| *m != "btree").map(|m| format!(" USING {m}")).unwrap_or_default();
        // Index columns may be expressions, which pg_get_indexdef already writes as SQL; names come quoted as needed.
        out.push(format!("CREATE {unique}INDEX {} ON {t}{using} ({});", d.ident(&i.name), i.columns.join(", ")));
    }
    if let Some(c) = &s.comment {
        out.push(format!("COMMENT ON TABLE {t} IS {};", d.literal(c)));
    }
    for c in &s.columns {
        if let Some(comment) = &c.comment {
            out.push(format!("COMMENT ON COLUMN {t}.{} IS {};", d.ident(&c.name), d.literal(comment)));
        }
    }
    out.extend(s.triggers.iter().map(|tr| format!("{};", tr.definition)));
    out.join("\n\n")
}

/// SQLite's CREATE TABLE text (as `sqlite_master` keeps it) for a copy named `to`. SQLite stores the statement
/// as typed after `CREATE TABLE `, so the name is the first token there, bare or quoted.
fn sqlite_renamed_create(sql: &str, to: &str) -> Result<String> {
    const PREFIX: &str = "CREATE TABLE ";
    let rest = sql.strip_prefix(PREFIX).ok_or(Error::Invalid("unexpected CREATE TABLE text in sqlite_master"))?;
    let close = match rest.chars().next() {
        Some('"') => Some('"'),
        Some('`') => Some('`'),
        Some('[') => Some(']'),
        _ => None,
    };
    let end = match close {
        Some(q) => {
            // Skip doubled quotes inside the name.
            let mut i = 1;
            loop {
                let Some(at) = rest[i..].find(q).map(|p| p + i) else {
                    return Err(Error::Invalid("unexpected CREATE TABLE text in sqlite_master"));
                };
                if q != ']' && rest[at + 1..].starts_with(q) {
                    i = at + 2;
                } else {
                    break at + 1;
                }
            }
        }
        None => rest.find(|c: char| c.is_whitespace() || c == '(').unwrap_or(rest.len()),
    };
    Ok(format!("{PREFIX}{}{}", Dialect::Lite.ident(to), &rest[end..]))
}

impl Conn {
    /// The statements for structure `changes` to `table`; `create` builds a new table instead.
    pub async fn alter_sql(&mut self, table: &str, create: bool, changes: &[Change]) -> Result<Vec<String>> {
        let current = if create { None } else { Some(self.structure(table).await?) };
        alter_sql(self.dialect(), table, current.as_ref(), changes)
    }

    pub(crate) async fn is_view(&mut self, table: &str) -> Result<bool> {
        let rows = match self {
            Conn::Pg(_) => {
                let (schema, name) = table.split_once('.').unwrap_or(("public", table));
                self.fetch_with(
                    "select table_type::text from information_schema.tables where table_schema = $1 and table_name = $2",
                    &[Some(schema), Some(name)],
                )
                .await?
            }
            Conn::My(_) => {
                self.fetch_with(
                    "select table_type from information_schema.tables where table_schema = database() and table_name = ?",
                    &[Some(table)],
                )
                .await?
            }
            Conn::Lite(_) => self.fetch_with("select type from sqlite_master where name = $1", &[Some(table)]).await?,
        };
        Ok(rows
            .into_iter()
            .next()
            .and_then(|mut r| r.pop().flatten())
            .is_some_and(|t| t.to_lowercase().contains("view")))
    }

    async fn has_sqlite_sequence(&mut self) -> Result<bool> {
        Ok(!self.fetch_with("select 1 from sqlite_master where name = 'sqlite_sequence'", &[]).await?.is_empty())
    }

    /// The statements for a table operation.
    pub async fn op_sql(&mut self, op: &TableOp) -> Result<Vec<String>> {
        let d = self.dialect();
        let pg_only = || Error::Invalid("only Postgres has schemas here; MySQL's are its databases");
        Ok(match op {
            TableOp::Rename { table, to } => {
                vec![rename_sql(d, &d.table_ident(table), not_blank(to, "the table needs a name")?)]
            }
            TableOp::Clone { table, to, data } => self.clone_sql(table, to, *data).await?,
            TableOp::Truncate { table, cascade, restart_identity, ignore_fk } => {
                let t = d.table_ident(table);
                match d {
                    Dialect::Pg => vec![format!(
                        "TRUNCATE TABLE {t}{}{}",
                        if *restart_identity { " RESTART IDENTITY" } else { "" },
                        if *cascade { " CASCADE" } else { "" }
                    )],
                    Dialect::My => without_fk_checks(*ignore_fk, format!("TRUNCATE TABLE {t}")),
                    Dialect::Lite => {
                        let mut out = vec![format!("DELETE FROM {t}")];
                        if *restart_identity && self.has_sqlite_sequence().await? {
                            out.push(format!("DELETE FROM sqlite_sequence WHERE name = {}", d.literal(table)));
                        }
                        out
                    }
                }
            }
            TableOp::Drop { table, cascade, ignore_fk } => {
                let kind = if self.is_view(table).await? { "VIEW" } else { "TABLE" };
                let t = d.table_ident(table);
                match d {
                    Dialect::Pg => vec![format!("DROP {kind} {t}{}", if *cascade { " CASCADE" } else { "" })],
                    Dialect::My => without_fk_checks(*ignore_fk, format!("DROP {kind} {t}")),
                    // Foreign keys aren't enforced on this app's SQLite connections, so nothing to ignore.
                    Dialect::Lite => vec![format!("DROP {kind} {t}")],
                }
            }
            TableOp::ResetAutoIncrement { table } => self.reset_auto_increment_sql(table).await?,
            TableOp::CreateSchema { name } if d == Dialect::Pg => {
                vec![format!("CREATE SCHEMA {}", d.ident(not_blank(name, "the schema needs a name")?))]
            }
            TableOp::RenameSchema { name, to } if d == Dialect::Pg => {
                vec![format!(
                    "ALTER SCHEMA {} RENAME TO {}",
                    d.ident(name),
                    d.ident(not_blank(to, "the schema needs a name")?)
                )]
            }
            TableOp::DropSchema { name, cascade } if d == Dialect::Pg => {
                vec![format!("DROP SCHEMA {}{}", d.ident(name), if *cascade { " CASCADE" } else { "" })]
            }
            TableOp::CreateSchema { .. } | TableOp::RenameSchema { .. } | TableOp::DropSchema { .. } => {
                return Err(pg_only());
            }
        })
    }

    /// The statements for a database operation.
    pub async fn database_sql(&mut self, op: &DbOp) -> Result<Vec<String>> {
        let d = self.dialect();
        if d == Dialect::Lite {
            return Err(Error::Invalid("a SQLite file is its only database"));
        }
        Ok(match op {
            DbOp::Create { name, encoding, collation } => {
                vec![create_database_sql(d, name, blank_is_none(encoding.as_ref()), blank_is_none(collation.as_ref()))?]
            }
            DbOp::Rename { name, to } => {
                let to = not_blank(to, "the database needs a name")?;
                if d == Dialect::Pg {
                    vec![format!("ALTER DATABASE {} RENAME TO {}", d.ident(name), d.ident(to))]
                } else {
                    self.mysql_rename_database_sql(name, to).await?
                }
            }
            DbOp::Drop { name } => vec![format!("DROP DATABASE {}", d.ident(name))],
        })
    }

    /// MySQL has no RENAME DATABASE: a new database with the old one's character set and collation, every table
    /// moved over with RENAME TABLE, then the emptied old one dropped. Views, routines, triggers and events don't
    /// move that way, so a database holding any is refused rather than have DROP DATABASE take them along.
    // ponytail: a table created between this preview and running it would go with the DROP; lock or re-check
    // at run time if that ever matters.
    async fn mysql_rename_database_sql(&mut self, name: &str, to: &str) -> Result<Vec<String>> {
        let d = Dialect::My;
        let others = self
            .fetch_with(
                "select (select count(*) from information_schema.tables where table_schema = ? \
                 and table_type <> 'BASE TABLE') + (select count(*) from information_schema.routines \
                 where routine_schema = ?) + (select count(*) from information_schema.triggers \
                 where trigger_schema = ?) + (select count(*) from information_schema.events where event_schema = ?)",
                &[Some(name); 4],
            )
            .await?;
        if others.first().and_then(|r| r.first()).cloned().flatten().as_deref() != Some("0") {
            return Err(Error::Invalid(
                "MySQL can't rename a database; tables can be moved to a new one, but this one also has views, \
                 routines, triggers or events, which can't",
            ));
        }
        let rows = self
            .fetch_with(
                "select default_character_set_name, default_collation_name from information_schema.schemata \
                 where schema_name = ?",
                &[Some(name)],
            )
            .await?;
        let [Some(charset), Some(collation)] = rows.first().map(Vec::as_slice).unwrap_or_default() else {
            return Err(Error::Invalid("no such database"));
        };
        let tables = self
            .fetch_with(
                "select table_name from information_schema.tables where table_schema = ? order by 1",
                &[Some(name)],
            )
            .await?;
        let (from, into) = (d.ident(name), d.ident(to));
        let mut out = vec![format!(
            "CREATE DATABASE {into} CHARACTER SET {} COLLATE {}",
            d.literal(charset),
            d.literal(collation)
        )];
        let moves: Vec<String> = tables
            .into_iter()
            .filter_map(|mut r| r.swap_remove(0))
            .map(|t| format!("{from}.{t} TO {into}.{t}", t = d.ident(&t)))
            .collect();
        if !moves.is_empty() {
            out.push(format!("RENAME TABLE {}", moves.join(", ")));
        }
        out.push(format!("DROP DATABASE {from}"));
        Ok(out)
    }

    /// The encodings and collations the server offers for a new database.
    pub async fn database_options(&mut self) -> Result<DbOptions> {
        let (encodings, collations) = match self.dialect() {
            Dialect::Pg => (
                // The server's encoding numbers run up to the low 40s; the client-only ones are refused on create.
                "select pg_encoding_to_char(i) from generate_series(0, 63) i where pg_encoding_to_char(i) <> '' \
                 order by 1",
                "select distinct collcollate, '' from pg_collation where collcollate is not null order by 1",
            ),
            Dialect::My => (
                "select character_set_name from information_schema.character_sets order by 1",
                "select collation_name, character_set_name from information_schema.collations order by 1",
            ),
            Dialect::Lite => return Err(Error::Invalid("a SQLite file is its only database")),
        };
        let encodings = self.fetch_with(encodings, &[]).await?.into_iter().filter_map(|mut r| r.swap_remove(0));
        let collations = self.fetch_with(collations, &[]).await?.into_iter().filter_map(|mut r| {
            r.resize(2, None);
            Some((r[0].take()?, r[1].take().unwrap_or_default()))
        });
        Ok(DbOptions { encodings: encodings.collect(), collations: collations.collect() })
    }

    async fn clone_sql(&mut self, table: &str, to: &str, data: bool) -> Result<Vec<String>> {
        let d = self.dialect();
        let to = not_blank(to, "the copy needs a name")?;
        // A Postgres copy goes in the original's schema unless one is given.
        let to = match (d, table.split_once('.'), to.contains('.')) {
            (Dialect::Pg, Some((schema, _)), false) => format!("{schema}.{to}"),
            _ => to.to_string(),
        };
        let (from, new) = (d.table_ident(table), d.table_ident(&to));
        let create = match d {
            // ponytail: INCLUDING ALL copies serial defaults, so the copy shares the original's sequence;
            // give it its own sequence if that matters.
            Dialect::Pg => format!("CREATE TABLE {new} (LIKE {from} INCLUDING ALL)"),
            Dialect::My => format!("CREATE TABLE {new} LIKE {from}"),
            Dialect::Lite => {
                let sql = self
                    .fetch_with("select sql from sqlite_master where type = 'table' and name = $1", &[Some(table)])
                    .await?
                    .into_iter()
                    .next()
                    .and_then(|mut r| r.pop().flatten())
                    .ok_or(Error::Invalid("no such table"))?;
                // ponytail: indexes and triggers aren't copied on SQLite; copy them from the DDL if wanted.
                sqlite_renamed_create(&sql, &to)?
            }
        };
        let mut out = vec![create];
        if data {
            out.push(format!("INSERT INTO {new} SELECT * FROM {from}"));
        }
        Ok(out)
    }

    async fn reset_auto_increment_sql(&mut self, table: &str) -> Result<Vec<String>> {
        let d = self.dialect();
        let t = d.table_ident(table);
        Ok(match d {
            // To one past the largest value, so the next insert can't collide with an existing row.
            Dialect::Pg => {
                let s = self.structure(table).await?;
                let out: Vec<String> = s
                    .columns
                    .iter()
                    .filter(|c| c.auto_increment)
                    .map(|c| {
                        let col = d.ident(&c.name);
                        format!(
                            "SELECT setval(pg_get_serial_sequence({}, {}), coalesce(max({col}), 0) + 1, false) FROM {t}",
                            d.literal(&t),
                            d.literal(&c.name)
                        )
                    })
                    .collect();
                if out.is_empty() {
                    return Err(Error::Invalid("this table has no auto-increment column"));
                }
                out
            }
            // InnoDB raises a value below the largest one to one past it.
            Dialect::My => vec![format!("ALTER TABLE {t} AUTO_INCREMENT = 1")],
            Dialect::Lite => {
                if !self.has_sqlite_sequence().await? {
                    return Err(Error::Invalid(
                        "this database has no AUTOINCREMENT tables; a rowid always continues from the largest one",
                    ));
                }
                // Without its sqlite_sequence row, AUTOINCREMENT continues from the largest rowid.
                vec![format!("DELETE FROM sqlite_sequence WHERE name = {}", d.literal(table))]
            }
        })
    }

    /// The CREATE statement of a table or view, with its indexes and triggers where the engine keeps them apart.
    pub async fn ddl(&mut self, table: &str) -> Result<String> {
        let first =
            |rows: Vec<Vec<Option<String>>>, i: usize| rows.into_iter().next().and_then(|mut r| r.get_mut(i)?.take());
        match self {
            Conn::My(_) => {
                let sql = format!("show create table {}", self.table_ident(table));
                let rows = self.query(&sql).await?.rows;
                first(rows, 1).map(|s| format!("{s};")).ok_or(Error::Invalid("no such table"))
            }
            Conn::Lite(_) => {
                let rows = self
                    .fetch_with(
                        "select sql || ';' from sqlite_master where tbl_name = $1 and sql is not null \
                         order by type not in ('table', 'view'), type, name",
                        &[Some(table)],
                    )
                    .await?;
                let all: Vec<String> = rows.into_iter().filter_map(|mut r| r.pop().flatten()).collect();
                if all.is_empty() { Err(Error::Invalid("no such table")) } else { Ok(all.join("\n\n")) }
            }
            Conn::Pg(_) => {
                if self.is_view(table).await? {
                    let (schema, name) = table.split_once('.').unwrap_or(("public", table));
                    let rows = self
                        .fetch_with(
                            "select pg_get_viewdef(c.oid, true) from pg_class c join pg_namespace n on n.oid = c.relnamespace \
                             where n.nspname = $1 and c.relname = $2",
                            &[Some(schema), Some(name)],
                        )
                        .await?;
                    let body = first(rows, 0).ok_or(Error::Invalid("no such view"))?;
                    return Ok(format!("CREATE VIEW {} AS\n{}", self.table_ident(table), body.trim_start()));
                }
                let s = self.structure(table).await?;
                Ok(pg_create_sql(table, &s))
            }
        }
    }

    /// Runs statements generated above. Postgres and SQLite run them in one transaction. MySQL commits each DDL
    /// statement as it goes, so it runs them one by one and, on an error, says which had already run. A single
    /// statement runs on its own: it needs no transaction, and Postgres's CREATE and DROP DATABASE refuse one.
    pub async fn run_ddl(&mut self, statements: &[String]) -> Result<()> {
        if !matches!(self, Conn::My(_)) && statements.len() > 1 {
            self.query("begin").await?;
            for s in statements {
                if let Err(e) = self.query(s).await {
                    // The statement's own error is the useful one; a failed rollback ends with the session anyway.
                    let _ = self.query("rollback").await;
                    return Err(e);
                }
            }
            self.query("commit").await?;
            return Ok(());
        }
        for (i, s) in statements.iter().enumerate() {
            if let Err(e) = self.query(s).await {
                // Trailing SET statements restore session settings (foreign key checks); run them regardless.
                for rest in statements[i + 1..].iter().filter(|r| r.starts_with("SET ")) {
                    // Best effort: the original error is what the user needs to see.
                    let _ = self.query(rest).await;
                }
                if i == 0 {
                    return Err(e);
                }
                return Err(Error::DdlPartlyApplied { ran: statements[..i].join(";\n"), source: Box::new(e) });
            }
        }
        Ok(())
    }
}

impl Conn {
    /// [`Conn::run_ddl`], and when MySQL fails partway (a routine dropped to be created again, whose new definition
    /// failed), runs `restore` to put back what was there.
    pub async fn run_ddl_or_restore(&mut self, statements: &[String], restore: &[String]) -> Result<()> {
        match self.run_ddl(statements).await {
            Err(Error::DdlPartlyApplied { source, .. }) if !restore.is_empty() => {
                Err(match self.run_ddl(restore).await {
                    Ok(()) => Error::DdlRestored(source),
                    Err(e) => Error::DdlRestoreFailed { source, restore: Box::new(e), original: restore.join(";\n\n") },
                })
            }
            r => r,
        }
    }
}

/// MySQL: `statement` with foreign key checks off around it, when asked.
fn without_fk_checks(ignore: bool, statement: String) -> Vec<String> {
    if ignore {
        vec!["SET FOREIGN_KEY_CHECKS = 0".into(), statement, "SET FOREIGN_KEY_CHECKS = 1".into()]
    } else {
        vec![statement]
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Change, ColumnDef, DbOp, IndexMethod, Rule, TableOp, TriggerEvent, TriggerTiming, alter_sql,
        create_database_sql, pg_create_sql, sqlite_renamed_create,
    };
    use crate::db::{Conn, Dialect, Engine, Options};
    use crate::error::Error;
    use crate::structure::{ColumnInfo, ForeignKeyInfo, IndexInfo, NamedDef, Structure, TriggerInfo};
    use crate::tls::Tls;

    fn col(name: &str, data_type: &str) -> ColumnDef {
        ColumnDef {
            name: name.into(),
            data_type: data_type.into(),
            nullable: true,
            default: None,
            comment: None,
            auto_increment: false,
            primary: false,
        }
    }

    fn info(name: &str, data_type: &str) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: data_type.into(),
            nullable: true,
            default: None,
            comment: None,
            auto_increment: false,
            primary: false,
            on_update: None,
        }
    }

    fn structure(columns: Vec<ColumnInfo>) -> Structure {
        Structure {
            columns,
            indexes: vec![],
            foreign_keys: vec![],
            checks: vec![],
            triggers: vec![],
            comment: None,
            options: None,
        }
    }

    #[test]
    fn literal_should_escape_quotes_everywhere_and_backslashes_on_postgres_and_mysql() {
        assert_eq!(Dialect::Pg.literal(r"it's a \ b"), r"E'it''s a \\ b'");
        assert_eq!(Dialect::My.literal(r"it's a \ b"), r"'it''s a \\ b'");
        assert_eq!(Dialect::Lite.literal(r"it's a \ b"), r"'it''s a \ b'");
        assert_eq!(Dialect::My.literal(r"\'; drop table t; --"), r"'\\''; drop table t; --'");
    }

    #[test]
    fn alter_sql_should_write_each_postgres_column_change_and_quote_names() {
        let current = structure(vec![info("old name", "text")]);
        let want = ColumnDef {
            nullable: false,
            default: Some("'x'".into()),
            comment: Some("it's".into()),
            ..col("new\"name", "integer")
        };
        let sql = alter_sql(
            Dialect::Pg,
            "public.t",
            Some(&current),
            &[Change::ModifyColumn { from: "old name".into(), column: want }],
        )
        .unwrap();
        assert_eq!(
            sql,
            [
                r#"ALTER TABLE "public"."t" RENAME COLUMN "old name" TO "new""name""#,
                r#"ALTER TABLE "public"."t" ALTER COLUMN "new""name" TYPE integer USING "new""name"::integer"#,
                r#"ALTER TABLE "public"."t" ALTER COLUMN "new""name" SET NOT NULL"#,
                r#"ALTER TABLE "public"."t" ALTER COLUMN "new""name" SET DEFAULT 'x'"#,
                r#"COMMENT ON COLUMN "public"."t"."new""name" IS E'it''s'"#,
            ]
        );
    }

    #[test]
    fn alter_sql_should_write_nothing_for_an_unchanged_column() {
        let current = structure(vec![info("a", "text")]);
        let same = ColumnDef { default: Some("  ".into()), comment: Some(String::new()), ..col("a", "text") };
        for d in [Dialect::Pg, Dialect::My, Dialect::Lite] {
            let changes = [Change::ModifyColumn { from: "a".into(), column: same.clone() }];
            assert!(alter_sql(d, "t", Some(&current), &changes).unwrap().is_empty(), "{d:?}");
        }
    }

    #[test]
    fn alter_sql_should_restate_the_whole_mysql_column_and_keep_on_update() {
        let current = structure(vec![ColumnInfo {
            default: Some("CURRENT_TIMESTAMP".into()),
            on_update: Some("CURRENT_TIMESTAMP".into()),
            ..info("at", "timestamp")
        }]);
        let want = ColumnDef {
            default: Some("CURRENT_TIMESTAMP".into()),
            comment: Some("set by `db`".into()),
            ..col("at", "timestamp")
        };
        let sql =
            alter_sql(Dialect::My, "t", Some(&current), &[Change::ModifyColumn { from: "at".into(), column: want }])
                .unwrap();
        assert_eq!(
            sql,
            [
                "ALTER TABLE `t` CHANGE COLUMN `at` `at` timestamp NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP \
             COMMENT 'set by `db`'"
            ]
        );
    }

    fn add_index(method: Option<IndexMethod>) -> Change {
        Change::AddIndex { name: "i".into(), columns: vec!["a".into()], unique: false, method }
    }

    fn primary_index(name: &str) -> IndexInfo {
        IndexInfo {
            name: name.into(),
            columns: vec!["id".into()],
            unique: true,
            primary: true,
            method: None,
            constraint: true,
        }
    }

    #[test]
    fn alter_sql_should_put_the_index_method_where_each_engine_wants_it() {
        let current = structure(vec![info("a", "text")]);
        let pg = alter_sql(Dialect::Pg, "s.t", Some(&current), &[add_index(Some(IndexMethod::Gin))]).unwrap();
        assert_eq!(pg, [r#"CREATE INDEX "i" ON "s"."t" USING gin ("a")"#]);
        let my = alter_sql(Dialect::My, "t", Some(&current), &[add_index(Some(IndexMethod::Hash))]).unwrap();
        assert_eq!(my, ["CREATE INDEX `i` ON `t` (`a`) USING HASH"]);
        let default = alter_sql(Dialect::Lite, "t", Some(&current), &[add_index(None)]).unwrap();
        assert_eq!(default, [r#"CREATE INDEX "i" ON "t" ("a")"#]);
    }

    #[test]
    fn alter_sql_should_refuse_an_index_method_the_engine_lacks() {
        let current = structure(vec![info("a", "text")]);
        assert!(matches!(
            alter_sql(Dialect::My, "t", Some(&current), &[add_index(Some(IndexMethod::Gist))]),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            alter_sql(Dialect::Lite, "t", Some(&current), &[add_index(Some(IndexMethod::Btree))]),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn alter_sql_should_add_and_drop_named_checks() {
        let current = structure(vec![info("a", "integer")]);
        let changes = [
            Change::AddCheck { name: "a_positive".into(), expression: "a > 0".into() },
            Change::DropCheck { name: "old check".into() },
        ];
        assert_eq!(
            alter_sql(Dialect::Pg, "t", Some(&current), &changes).unwrap(),
            [
                r#"ALTER TABLE "t" ADD CONSTRAINT "a_positive" CHECK (a > 0)"#,
                r#"ALTER TABLE "t" DROP CONSTRAINT "old check""#
            ]
        );
        assert_eq!(
            alter_sql(Dialect::My, "t", Some(&current), &changes).unwrap(),
            [
                "ALTER TABLE `t` ADD CONSTRAINT `a_positive` CHECK (a > 0)",
                "ALTER TABLE `t` DROP CONSTRAINT `old check`"
            ]
        );
    }

    #[test]
    fn alter_sql_should_refuse_a_blank_check_and_sqlite_check_changes() {
        let current = structure(vec![info("a", "integer")]);
        let blank = [Change::AddCheck { name: String::new(), expression: "  ".into() }];
        assert!(matches!(alter_sql(Dialect::Pg, "t", Some(&current), &blank), Err(Error::Invalid(_))));
        let add = [Change::AddCheck { name: String::new(), expression: "a > 0".into() }];
        assert!(matches!(alter_sql(Dialect::Lite, "t", Some(&current), &add), Err(Error::SqliteRebuild(_))));
    }

    #[test]
    fn alter_sql_should_replace_a_mysql_primary_key_in_one_statement() {
        let mut current = structure(vec![info("id", "int"), info("b", "int")]);
        current.indexes = vec![primary_index("PRIMARY")];
        let set = |columns: &[&str]| [Change::SetPrimaryKey { columns: columns.iter().map(|c| (*c).into()).collect() }];
        assert_eq!(
            alter_sql(Dialect::My, "t", Some(&current), &set(&["id", "b"])).unwrap(),
            ["ALTER TABLE `t` DROP PRIMARY KEY, ADD PRIMARY KEY (`id`, `b`)"]
        );
        assert_eq!(
            alter_sql(Dialect::My, "t", Some(&current), &set(&[])).unwrap(),
            ["ALTER TABLE `t` DROP PRIMARY KEY"]
        );
        current.indexes.clear();
        assert_eq!(alter_sql(Dialect::My, "t", Some(&current), &set(&[])).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn alter_sql_should_drop_the_postgres_primary_key_constraint_by_name_then_add_the_new_one() {
        let mut current = structure(vec![info("id", "int"), info("b", "int")]);
        current.indexes = vec![primary_index("t_pkey")];
        let changes = [Change::SetPrimaryKey { columns: vec!["b".into()] }];
        assert_eq!(
            alter_sql(Dialect::Pg, "s.t", Some(&current), &changes).unwrap(),
            [r#"ALTER TABLE "s"."t" DROP CONSTRAINT "t_pkey""#, r#"ALTER TABLE "s"."t" ADD PRIMARY KEY ("b")"#]
        );
        assert!(matches!(alter_sql(Dialect::Lite, "t", Some(&current), &changes), Err(Error::SqliteRebuild(_))));
    }

    fn add_trigger(events: Vec<TriggerEvent>, body: &str) -> Change {
        Change::AddTrigger { name: "touch".into(), timing: TriggerTiming::Before, events, body: body.into() }
    }

    #[test]
    fn alter_sql_should_write_each_engines_row_trigger() {
        let current = structure(vec![info("a", "int")]);
        let pg = [add_trigger(vec![TriggerEvent::Insert, TriggerEvent::Update], "touch()")];
        assert_eq!(
            alter_sql(Dialect::Pg, "s.t", Some(&current), &pg).unwrap(),
            [r#"CREATE TRIGGER "touch" BEFORE INSERT OR UPDATE ON "s"."t" FOR EACH ROW EXECUTE FUNCTION touch()"#]
        );
        let my = [add_trigger(vec![TriggerEvent::Delete], "SET @n = @n + 1")];
        assert_eq!(
            alter_sql(Dialect::My, "t", Some(&current), &my).unwrap(),
            ["CREATE TRIGGER `touch` BEFORE DELETE ON `t` FOR EACH ROW SET @n = @n + 1"]
        );
        let lite = [add_trigger(vec![TriggerEvent::Insert], "select 1")];
        assert_eq!(
            alter_sql(Dialect::Lite, "t", Some(&current), &lite).unwrap(),
            ["CREATE TRIGGER \"touch\" BEFORE INSERT ON \"t\" FOR EACH ROW BEGIN\n  select 1;\nEND"]
        );
    }

    #[test]
    fn alter_sql_should_refuse_several_events_on_mysql_and_sqlite_and_a_blank_body() {
        let current = structure(vec![info("a", "int")]);
        let two = [add_trigger(vec![TriggerEvent::Insert, TriggerEvent::Update], "select 1")];
        assert!(matches!(alter_sql(Dialect::My, "t", Some(&current), &two), Err(Error::Invalid(_))));
        assert!(matches!(alter_sql(Dialect::Lite, "t", Some(&current), &two), Err(Error::Invalid(_))));
        let blank = [add_trigger(vec![TriggerEvent::Insert], " ")];
        assert!(matches!(alter_sql(Dialect::Pg, "t", Some(&current), &blank), Err(Error::Invalid(_))));
        let none = [add_trigger(vec![], "f()")];
        assert!(matches!(alter_sql(Dialect::Pg, "t", Some(&current), &none), Err(Error::Invalid(_))));
    }

    #[test]
    fn alter_sql_should_drop_a_postgres_trigger_on_its_table() {
        let current = structure(vec![info("a", "int")]);
        let drop = [Change::DropTrigger { name: "touch".into() }];
        assert_eq!(
            alter_sql(Dialect::Pg, "s.t", Some(&current), &drop).unwrap(),
            [r#"DROP TRIGGER "touch" ON "s"."t""#]
        );
        assert_eq!(alter_sql(Dialect::My, "t", Some(&current), &drop).unwrap(), ["DROP TRIGGER `touch`"]);
    }

    #[test]
    fn alter_sql_should_set_mysql_engine_and_collation_only_as_plain_names() {
        let current = structure(vec![info("a", "int")]);
        let ok = [Change::Engine { engine: "MyISAM".into() }, Change::Collation { collation: "utf8mb4_bin".into() }];
        assert_eq!(
            alter_sql(Dialect::My, "t", Some(&current), &ok).unwrap(),
            ["ALTER TABLE `t` ENGINE = MyISAM", "ALTER TABLE `t` COLLATE = utf8mb4_bin"]
        );
        let sneaky = [Change::Engine { engine: "InnoDB; drop table t".into() }];
        assert!(matches!(alter_sql(Dialect::My, "t", Some(&current), &sneaky), Err(Error::Invalid(_))));
        assert!(matches!(alter_sql(Dialect::Pg, "t", Some(&current), &ok), Err(Error::Invalid(_))));
    }

    #[test]
    fn create_sql_should_put_checks_inline_and_mysql_options_after_the_columns() {
        let changes = [
            Change::AddColumn { column: ColumnDef { primary: true, nullable: false, ..col("id", "int") } },
            Change::AddCheck { name: "id_positive".into(), expression: "id > 0".into() },
            Change::Engine { engine: "InnoDB".into() },
        ];
        assert_eq!(
            alter_sql(Dialect::My, "t", None, &changes).unwrap(),
            [
                "CREATE TABLE `t` (\n  `id` int NOT NULL,\n  PRIMARY KEY (`id`),\n  CONSTRAINT `id_positive` CHECK (id > 0)\n) ENGINE = InnoDB"
            ]
        );
    }

    #[test]
    fn alter_sql_should_run_the_table_rename_last() {
        let current = structure(vec![info("a", "text")]);
        let changes = [Change::Rename { to: "u".into() }, Change::DropColumn { name: "a".into() }];
        assert_eq!(
            alter_sql(Dialect::My, "t", Some(&current), &changes).unwrap(),
            ["ALTER TABLE `t` DROP COLUMN `a`", "RENAME TABLE `t` TO `u`"]
        );
        assert_eq!(
            alter_sql(Dialect::Pg, "s.t", Some(&current), &changes).unwrap()[1],
            r#"ALTER TABLE "s"."t" RENAME TO "u""#
        );
    }

    #[test]
    fn alter_sql_should_drop_postgres_constraint_indexes_as_constraints() {
        let mut current = structure(vec![info("a", "text")]);
        let index = |name: &str, constraint| IndexInfo {
            name: name.into(),
            columns: vec!["a".into()],
            unique: true,
            primary: false,
            method: None,
            constraint,
        };
        current.indexes = vec![index("t_a_key", true), index("t_a_idx", false)];
        let changes = [Change::DropIndex { name: "t_a_key".into() }, Change::DropIndex { name: "t_a_idx".into() }];
        assert_eq!(
            alter_sql(Dialect::Pg, "s.t", Some(&current), &changes).unwrap(),
            [r#"ALTER TABLE "s"."t" DROP CONSTRAINT "t_a_key""#, r#"DROP INDEX "s"."t_a_idx""#]
        );
    }

    #[test]
    fn alter_sql_should_say_sqlite_needs_a_rebuild_for_type_changes_and_foreign_keys() {
        let current = structure(vec![info("a", "text")]);
        let retype = [Change::ModifyColumn { from: "a".into(), column: col("a", "integer") }];
        assert!(matches!(alter_sql(Dialect::Lite, "t", Some(&current), &retype), Err(Error::SqliteRebuild(_))));
        let fk = [Change::DropForeignKey { name: "fk_0".into() }];
        assert!(matches!(alter_sql(Dialect::Lite, "t", Some(&current), &fk), Err(Error::SqliteRebuild(_))));
    }

    #[test]
    fn alter_sql_should_create_a_table_with_its_key_foreign_keys_and_indexes() {
        let changes = [
            Change::AddColumn {
                column: ColumnDef { nullable: false, auto_increment: true, primary: true, ..col("id", "bigint") },
            },
            Change::AddColumn { column: ColumnDef { comment: Some("who".into()), ..col("owner", "bigint") } },
            Change::AddForeignKey {
                name: "pets_owner".into(),
                columns: vec!["owner".into()],
                ref_table: "public.owners".into(),
                ref_columns: vec!["id".into()],
                on_update: Rule::NoAction,
                on_delete: Rule::Cascade,
            },
            Change::AddIndex {
                name: "pets_owner_idx".into(),
                columns: vec!["owner".into()],
                unique: false,
                method: None,
            },
        ];
        assert_eq!(
            alter_sql(Dialect::Pg, "public.pets", None, &changes).unwrap(),
            [
                "CREATE TABLE \"public\".\"pets\" (\n  \"id\" bigint GENERATED BY DEFAULT AS IDENTITY NOT NULL,\n  \
             \"owner\" bigint NULL,\n  PRIMARY KEY (\"id\"),\n  CONSTRAINT \"pets_owner\" FOREIGN KEY (\"owner\") \
             REFERENCES \"public\".\"owners\" (\"id\") ON UPDATE NO ACTION ON DELETE CASCADE\n)",
                r#"COMMENT ON COLUMN "public"."pets"."owner" IS E'who'"#,
                r#"CREATE INDEX "pets_owner_idx" ON "public"."pets" ("owner")"#,
            ]
        );
        let my = alter_sql(Dialect::My, "pets", None, &changes).unwrap();
        assert!(
            my[0].contains("`id` bigint NOT NULL AUTO_INCREMENT,\n  `owner` bigint NULL COMMENT 'who'"),
            "{}",
            my[0]
        );
    }

    #[test]
    fn alter_sql_should_refuse_a_new_table_without_columns_or_with_blank_names() {
        assert!(alter_sql(Dialect::Lite, "t", None, &[]).is_err());
        let blank = [Change::AddColumn { column: col(" ", "text") }];
        assert!(alter_sql(Dialect::Lite, "t", None, &blank).is_err());
        assert!(alter_sql(Dialect::Lite, "", None, &[Change::AddColumn { column: col("a", "text") }]).is_err());
    }

    #[test]
    fn change_should_reject_unknown_fields_and_actions() {
        let ok = r#"{"op":"add_index","name":"i","columns":["a"],"unique":false}"#;
        assert!(serde_json::from_str::<Change>(ok).is_ok());
        let extra = r#"{"op":"add_index","name":"i","columns":["a"],"unique":false,"sql":"x"}"#;
        assert!(serde_json::from_str::<Change>(extra).is_err());
        let fk = r#"{"op":"add_foreign_key","name":"f","columns":["a"],"ref_table":"r","ref_columns":["id"],
            "on_update":"NO ACTION","on_delete":"DROP TABLE"}"#;
        assert!(serde_json::from_str::<Change>(fk).is_err());
    }

    #[test]
    fn change_should_accept_what_the_structure_view_sends() {
        let sent = r#"[{"op":"add_column","column":{"name":"id","data_type":"bigint","nullable":false,"default":null,
            "comment":null,"auto_increment":true,"primary":true}},
            {"op":"modify_column","from":"a","column":{"name":"b","data_type":"text","nullable":true,"default":"'x'",
            "comment":"c","auto_increment":false,"primary":false}},
            {"op":"drop_column","name":"c"},{"op":"add_index","name":"i","columns":["id"],"unique":false},
            {"op":"drop_index","name":"j"},{"op":"add_foreign_key","name":"f","columns":["a"],"ref_table":"public.o",
            "ref_columns":["id"],"on_update":"NO ACTION","on_delete":"SET NULL"},{"op":"drop_foreign_key","name":"g"},
            {"op":"rename","to":"u"},{"op":"comment","comment":null},
            {"op":"add_index","name":"k","columns":["id"],"unique":true,"method":"gin"},
            {"op":"add_index","name":"m","columns":["id"],"unique":false,"method":null},
            {"op":"add_check","name":"c","expression":"a > 0"},{"op":"drop_check","name":"d"},
            {"op":"set_primary_key","columns":["id","a"]},{"op":"engine","engine":"InnoDB"},
            {"op":"collation","collation":"utf8mb4_bin"}]"#;
        assert_eq!(serde_json::from_str::<Vec<Change>>(sent).unwrap().len(), 16);
        let unknown_method = r#"[{"op":"add_index","name":"k","columns":["id"],"unique":true,"method":"rtree"}]"#;
        assert!(serde_json::from_str::<Vec<Change>>(unknown_method).is_err());
        let ops = r#"[{"op":"truncate","table":"t","cascade":false,"restart_identity":true,"ignore_fk":false},
            {"op":"drop","table":"t","cascade":true,"ignore_fk":false},{"op":"clone","table":"t","to":"u","data":true},
            {"op":"rename","table":"t","to":"u"},{"op":"reset_auto_increment","table":"t"},
            {"op":"create_schema","name":"s"},{"op":"rename_schema","name":"s","to":"r"},
            {"op":"drop_schema","name":"s","cascade":false}]"#;
        assert_eq!(serde_json::from_str::<Vec<TableOp>>(ops).unwrap().len(), 8);
    }

    #[test]
    fn sqlite_renamed_create_should_replace_bare_and_quoted_names() {
        let renamed = |sql: &str| sqlite_renamed_create(sql, "copy").unwrap();
        assert_eq!(renamed("CREATE TABLE pets(id integer)"), r#"CREATE TABLE "copy"(id integer)"#);
        assert_eq!(renamed(r#"CREATE TABLE "my ""pets"" (" (id)"#), r#"CREATE TABLE "copy" (id)"#);
        assert_eq!(renamed("CREATE TABLE [a b] (id)"), r#"CREATE TABLE "copy" (id)"#);
        assert!(sqlite_renamed_create("CREATE VIEW v AS select 1", "copy").is_err());
    }

    #[test]
    fn pg_create_sql_should_write_constraints_indexes_comments_and_triggers() {
        let s = Structure {
            columns: vec![
                ColumnInfo { nullable: false, primary: true, ..info("id", "integer") },
                ColumnInfo { comment: Some("who".into()), default: Some("0".into()), ..info("owner", "integer") },
            ],
            indexes: vec![
                IndexInfo {
                    name: "pets_pkey".into(),
                    columns: vec!["id".into()],
                    unique: true,
                    primary: true,
                    method: Some("btree".into()),
                    constraint: true,
                },
                IndexInfo {
                    name: "pets_tags".into(),
                    columns: vec!["lower(owner::text)".into()],
                    unique: false,
                    primary: false,
                    method: Some("gin".into()),
                    constraint: false,
                },
            ],
            foreign_keys: vec![ForeignKeyInfo {
                name: "pets_owner_fkey".into(),
                columns: vec!["owner".into()],
                ref_table: "public.owners".into(),
                ref_columns: vec!["id".into()],
                on_update: "NO ACTION".into(),
                on_delete: "SET NULL".into(),
            }],
            checks: vec![NamedDef { name: "owner_positive".into(), definition: "CHECK (owner > 0)".into() }],
            triggers: vec![TriggerInfo {
                name: "touch".into(),
                definition: "CREATE TRIGGER touch …".into(),
                ..TriggerInfo::default()
            }],
            comment: Some("pets".into()),
            options: None,
        };
        assert_eq!(
            pg_create_sql("public.pets", &s),
            "CREATE TABLE \"public\".\"pets\" (\n  \"id\" integer NOT NULL,\n  \"owner\" integer DEFAULT 0,\n  \
             CONSTRAINT \"pets_pkey\" PRIMARY KEY (\"id\"),\n  CONSTRAINT \"pets_owner_fkey\" FOREIGN KEY (\"owner\") \
             REFERENCES \"public\".\"owners\" (\"id\") ON UPDATE NO ACTION ON DELETE SET NULL,\n  \
             CONSTRAINT \"owner_positive\" CHECK (owner > 0)\n);\n\n\
             CREATE INDEX \"pets_tags\" ON \"public\".\"pets\" USING gin (lower(owner::text));\n\n\
             COMMENT ON TABLE \"public\".\"pets\" IS E'pets';\n\n\
             COMMENT ON COLUMN \"public\".\"pets\".\"owner\" IS E'who';\n\n\
             CREATE TRIGGER touch …;"
        );
    }

    #[test]
    fn create_database_sql_should_add_encoding_and_collation_per_engine() {
        assert_eq!(create_database_sql(Dialect::Pg, "shop", None, None).unwrap(), r#"CREATE DATABASE "shop""#);
        assert_eq!(
            create_database_sql(Dialect::Pg, "shop", Some("UTF8"), Some("C")).unwrap(),
            r#"CREATE DATABASE "shop" ENCODING E'UTF8' LC_COLLATE E'C' TEMPLATE template0"#
        );
        assert_eq!(
            create_database_sql(Dialect::My, "a`b", Some("utf8mb4"), Some("x' or '1")).unwrap(),
            "CREATE DATABASE `a``b` CHARACTER SET 'utf8mb4' COLLATE 'x'' or ''1'"
        );
        assert!(create_database_sql(Dialect::My, " ", None, None).is_err());
    }

    #[test]
    fn database_sql_should_refuse_sqlite_and_unknown_fields() {
        tauri::async_runtime::block_on(async {
            let err = sqlite().await.database_sql(&DbOp::Drop { name: "main".into() }).await.unwrap_err();
            assert_eq!(err.to_string(), "a SQLite file is its only database");
        });
        assert!(serde_json::from_str::<DbOp>(r#"{"op":"drop","name":"x","force":true}"#).is_err());
    }

    async fn sqlite() -> Conn {
        Conn::open(Engine::Sqlite, ":memory:", None, &Tls::default(), &Options::default(), None).await.unwrap()
    }

    async fn apply(c: &mut Conn, table: &str, create: bool, changes: &[Change]) -> crate::error::Result<()> {
        let sql = c.alter_sql(table, create, changes).await?;
        c.run_ddl(&sql).await
    }

    #[test]
    fn sqlite_structure_edits_should_create_then_alter_a_table() {
        tauri::async_runtime::block_on(async {
            let mut c = sqlite().await;
            let id = ColumnDef { nullable: false, primary: true, ..col("id", "integer") };
            apply(
                &mut c,
                "pets",
                true,
                &[Change::AddColumn { column: id }, Change::AddColumn { column: col("name", "text") }],
            )
            .await
            .unwrap();
            let changes = [
                Change::AddColumn { column: ColumnDef { default: Some("'?'".into()), ..col("kind", "text") } },
                Change::ModifyColumn { from: "name".into(), column: col("label", "text") },
                Change::AddIndex { name: "pets_kind".into(), columns: vec!["kind".into()], unique: true, method: None },
                Change::Rename { to: "animals".into() },
            ];
            apply(&mut c, "pets", false, &changes).await.unwrap();
            let s = c.structure("animals").await.unwrap();
            let names: Vec<_> = s.columns.iter().map(|c| c.name.as_str()).collect();
            assert_eq!(names, ["id", "label", "kind"]);
            assert!(s.columns[0].auto_increment, "INTEGER PRIMARY KEY is the rowid");
            assert_eq!(s.columns[2].default.as_deref(), Some("'?'"));
            assert!(s.indexes.iter().any(|i| i.name == "pets_kind" && i.unique));

            apply(&mut c, "animals", false, &[Change::DropColumn { name: "label".into() }]).await.unwrap();
            assert_eq!(c.structure("animals").await.unwrap().columns.len(), 2);
        });
    }

    #[test]
    fn run_ddl_should_roll_back_every_statement_when_one_fails_on_sqlite() {
        tauri::async_runtime::block_on(async {
            let mut c = sqlite().await;
            c.query("create table t(a text)").await.unwrap();
            let statements =
                ["ALTER TABLE t ADD COLUMN b text".to_string(), "ALTER TABLE nope ADD COLUMN c text".into()];
            assert!(c.run_ddl(&statements).await.is_err());
            assert_eq!(c.structure("t").await.unwrap().columns.len(), 1);
            assert_eq!(c.query("select 1").await.unwrap().rows[0][0].as_deref(), Some("1"), "no transaction left open");
        });
    }

    #[test]
    fn sqlite_table_ops_should_clone_truncate_reset_and_drop() {
        tauri::async_runtime::block_on(async {
            let mut c = sqlite().await;
            c.query("create table \"my pets\"(id integer primary key autoincrement, name text not null)")
                .await
                .unwrap();
            c.query("insert into \"my pets\"(name) values ('a'), ('b')").await.unwrap();
            let op = async |c: &mut Conn, op: TableOp| {
                let sql = c.op_sql(&op).await?;
                c.run_ddl(&sql).await
            };
            op(&mut c, TableOp::Clone { table: "my pets".into(), to: "copy".into(), data: true }).await.unwrap();
            assert_eq!(c.query("select count(*) from copy").await.unwrap().rows[0][0].as_deref(), Some("2"));
            assert!(!c.structure("copy").await.unwrap().columns[1].nullable, "the copy keeps its constraints");

            let truncate =
                TableOp::Truncate { table: "my pets".into(), cascade: false, restart_identity: true, ignore_fk: false };
            op(&mut c, truncate).await.unwrap();
            c.query("insert into \"my pets\"(name) values ('c')").await.unwrap();
            assert_eq!(c.query("select id from \"my pets\"").await.unwrap().rows[0][0].as_deref(), Some("1"));

            c.query("insert into copy(id, name) values (9, 'z')").await.unwrap();
            c.query("delete from copy where id = 9").await.unwrap();
            op(&mut c, TableOp::ResetAutoIncrement { table: "copy".into() }).await.unwrap();
            c.query("insert into copy(name) values ('d')").await.unwrap();
            assert_eq!(c.query("select max(id) from copy").await.unwrap().rows[0][0].as_deref(), Some("3"));

            op(&mut c, TableOp::Drop { table: "copy".into(), cascade: false, ignore_fk: false }).await.unwrap();
            assert!(c.structure("copy").await.is_err());
            let schema = op(&mut c, TableOp::CreateSchema { name: "s".into() }).await;
            assert!(schema.is_err(), "schemas are Postgres's");
        });
    }

    #[test]
    fn sqlite_ddl_should_list_the_table_then_its_indexes_and_triggers() {
        tauri::async_runtime::block_on(async {
            let mut c = sqlite().await;
            c.query("create table t(a text)").await.unwrap();
            c.query("create index t_a on t(a)").await.unwrap();
            c.query("create trigger t_touch after insert on t begin select 1; end").await.unwrap();
            let ddl = c.ddl("t").await.unwrap();
            assert_eq!(
                ddl,
                "CREATE TABLE t(a text);\n\nCREATE INDEX t_a on t(a);\n\n\
                 CREATE TRIGGER t_touch after insert on t begin select 1; end;"
            );
            assert!(c.ddl("nope").await.is_err());
        });
    }

    /// Runs in a scratch database, dropped afterwards whatever happens.
    #[test]
    #[ignore = "needs a local MySQL; creates and drops a scratch database"]
    fn local_mysql_structure_edits_and_table_ops() {
        tauri::async_runtime::block_on(async {
            let url = "mysql://root@127.0.0.1:3306";
            let mut c = Conn::open(Engine::Mysql, url, None, &Tls::default(), &Options::default(), None).await.unwrap();
            let db = format!("dbm_ddl_{}", std::process::id());
            c.query(&format!("create database {db}")).await.unwrap();
            c.use_database(&db).await.unwrap();
            let outcome = mysql_scenario(&mut c).await;
            c.query(&format!("drop database {db}")).await.unwrap();
            outcome.unwrap();
        });
    }

    #[test]
    #[ignore = "needs a local MySQL; creates, renames and drops scratch databases"]
    fn local_mysql_database_ops() {
        tauri::async_runtime::block_on(async {
            let url = "mysql://root@127.0.0.1:3306";
            let mut c = Conn::open(Engine::Mysql, url, None, &Tls::default(), &Options::default(), None).await.unwrap();
            let (a, b) = (format!("dbm_dbops_a_{}", std::process::id()), format!("dbm_dbops_b_{}", std::process::id()));
            let outcome = mysql_database_scenario(&mut c, &a, &b).await;
            for db in [&a, &b] {
                let _ = c.query(&format!("drop database if exists {db}")).await; // cleanup; gone already is fine
            }
            outcome.unwrap();
        });
    }

    async fn mysql_database_scenario(c: &mut Conn, a: &str, b: &str) -> crate::error::Result<()> {
        let options = c.database_options().await?;
        assert!(options.encodings.iter().any(|e| e == "utf8mb4"));
        assert!(options.collations.iter().any(|(c, e)| c == "utf8mb4_bin" && e == "utf8mb4"));
        let create =
            DbOp::Create { name: a.into(), encoding: Some("utf8mb4".into()), collation: Some("utf8mb4_bin".into()) };
        let sql = c.database_sql(&create).await?;
        c.run_ddl(&sql).await?;
        c.query(&format!("create table {a}.t1(id int primary key); create table {a}.t2(id int)")).await?;
        c.query(&format!("insert into {a}.t1 values (7)")).await?;

        let sql = c.database_sql(&DbOp::Rename { name: a.into(), to: b.into() }).await?;
        assert_eq!(sql.len(), 3, "{sql:?}");
        c.run_ddl(&sql).await?;
        assert!(!c.databases().await?.iter().any(|d| d == a));
        assert_eq!(c.query(&format!("select id from {b}.t1")).await?.rows[0][0].as_deref(), Some("7"));
        let collation =
            format!("select default_collation_name from information_schema.schemata where schema_name = '{b}'");
        assert_eq!(c.query(&collation).await?.rows[0][0].as_deref(), Some("utf8mb4_bin"));

        c.query(&format!("create view {b}.v as select * from {b}.t1")).await?;
        let refused = c.database_sql(&DbOp::Rename { name: b.into(), to: a.into() }).await;
        assert!(refused.is_err_and(|e| e.to_string().contains("views")));

        let sql = c.database_sql(&DbOp::Drop { name: b.into() }).await?;
        c.run_ddl(&sql).await?;
        assert!(!c.databases().await?.iter().any(|d| d == b));
        Ok(())
    }

    /// Creating runs outside a transaction, which Postgres requires. `DBM_PG_URL` points elsewhere than the
    /// default server.
    #[test]
    #[ignore = "needs a local Postgres; creates, renames and drops a scratch database"]
    fn local_postgres_database_ops() {
        tauri::async_runtime::block_on(async {
            let url =
                std::env::var("DBM_PG_URL").unwrap_or_else(|_| "postgres://postgres@127.0.0.1:5432/postgres".into());
            let mut c =
                Conn::open(Engine::Postgres, &url, None, &Tls::default(), &Options::default(), None).await.unwrap();
            let (a, b) = (format!("dbm_dbops_a_{}", std::process::id()), format!("dbm_dbops_b_{}", std::process::id()));
            let outcome = postgres_database_scenario(&mut c, &a, &b).await;
            for db in [&a, &b] {
                let _ = c.query(&format!("drop database if exists {db}")).await; // cleanup; gone already is fine
            }
            outcome.unwrap();
        });
    }

    async fn postgres_database_scenario(c: &mut Conn, a: &str, b: &str) -> crate::error::Result<()> {
        assert!(c.database_options().await?.encodings.iter().any(|e| e == "UTF8"));
        let create = DbOp::Create { name: a.into(), encoding: Some("UTF8".into()), collation: Some("C".into()) };
        let sql = c.database_sql(&create).await?;
        c.run_ddl(&sql).await?;
        let sql = c.database_sql(&DbOp::Rename { name: a.into(), to: b.into() }).await?;
        c.run_ddl(&sql).await?;
        let sql = c.database_sql(&DbOp::Drop { name: b.into() }).await?;
        c.run_ddl(&sql).await?;
        let left = c.databases().await?;
        assert!(!left.iter().any(|d| d == a || d == b), "{left:?}");
        Ok(())
    }

    /// Checks, a new primary key, an index method, triggers and table options, in a scratch database dropped after.
    #[test]
    #[ignore = "needs a local MySQL 8.0.19+ or MariaDB; creates and drops a scratch database"]
    fn local_mysql_checks_keys_triggers_and_options() {
        tauri::async_runtime::block_on(async {
            let url = "mysql://root@127.0.0.1:3306";
            let mut c = Conn::open(Engine::Mysql, url, None, &Tls::default(), &Options::default(), None).await.unwrap();
            let db = format!("dbm_keys_{}", std::process::id());
            c.query(&format!("create database {db}")).await.unwrap();
            c.use_database(&db).await.unwrap();
            let outcome = mysql_keys_scenario(&mut c).await;
            c.query(&format!("drop database {db}")).await.unwrap();
            outcome.unwrap();
        });
    }

    async fn mysql_keys_scenario(c: &mut Conn) -> crate::error::Result<()> {
        let create = [
            Change::AddColumn { column: ColumnDef { nullable: false, primary: true, ..col("id", "int") } },
            Change::AddColumn { column: ColumnDef { nullable: false, ..col("n", "int") } },
            Change::AddCheck { name: "n_small".into(), expression: "n < 100".into() },
            Change::Engine { engine: "InnoDB".into() },
            Change::Collation { collation: "utf8mb4_bin".into() },
        ];
        apply(c, "t", true, &create).await?;
        let s = c.structure("t").await?;
        assert_eq!(s.checks.iter().map(|k| k.name.as_str()).collect::<Vec<_>>(), ["n_small"]);
        let options = s.options.as_ref().expect("MySQL tables have options");
        assert_eq!((options.engine.as_deref(), options.collation.as_deref()), (Some("InnoDB"), Some("utf8mb4_bin")));
        assert!(options.engines.iter().any(|e| e == "InnoDB") && options.collations.iter().any(|c| c == "utf8mb4_bin"));
        assert!(c.query("insert into t values (1, 500)").await.is_err(), "the check holds");

        let changes = [
            Change::DropCheck { name: "n_small".into() },
            Change::AddCheck { name: "n_positive".into(), expression: "n > 0".into() },
            Change::SetPrimaryKey { columns: vec!["id".into(), "n".into()] },
            Change::AddIndex {
                name: "t_n".into(),
                columns: vec!["n".into()],
                unique: false,
                method: Some(IndexMethod::Btree),
            },
            Change::Collation { collation: "utf8mb4_general_ci".into() },
        ];
        apply(c, "t", false, &changes).await?;
        let s = c.structure("t").await?;
        assert_eq!(s.checks.iter().map(|k| k.name.as_str()).collect::<Vec<_>>(), ["n_positive"]);
        assert_eq!(
            s.indexes.iter().find(|i| i.primary).map(|i| i.columns.clone()),
            Some(vec!["id".into(), "n".into()])
        );
        assert_eq!(s.indexes.iter().find(|i| i.name == "t_n").and_then(|i| i.method.as_deref()), Some("BTREE"));
        assert_eq!(s.options.and_then(|o| o.collation).as_deref(), Some("utf8mb4_general_ci"));
        c.query("insert into t values (1, 500)").await?;

        // A trigger with a compound body, then changed (drop and add) and dropped.
        let body = "BEGIN\n  SET NEW.n = NEW.n + 1;\nEND";
        let add = Change::AddTrigger {
            name: "t_bump".into(),
            timing: TriggerTiming::Before,
            events: vec![TriggerEvent::Insert],
            body: body.into(),
        };
        apply(c, "t", false, &[add]).await?;
        c.query("insert into t values (2, 5)").await?;
        assert_eq!(c.query("select n from t where id = 2").await?.rows[0][0].as_deref(), Some("6"));
        let s = c.structure("t").await?;
        let tr = &s.triggers[0];
        assert_eq!(
            (tr.timing.as_deref(), tr.events.clone(), tr.body.as_deref()),
            (Some("BEFORE"), vec!["INSERT".into()], Some(body))
        );
        let edit = [
            Change::DropTrigger { name: "t_bump".into() },
            Change::AddTrigger {
                name: "t_bump".into(),
                timing: TriggerTiming::Before,
                events: vec![TriggerEvent::Insert],
                body: "SET NEW.n = 1".into(),
            },
        ];
        apply(c, "t", false, &edit).await?;
        c.query("insert into t values (3, 5)").await?;
        assert_eq!(c.query("select n from t where id = 3").await?.rows[0][0].as_deref(), Some("1"));
        apply(c, "t", false, &[Change::DropTrigger { name: "t_bump".into() }]).await?;
        assert!(c.structure("t").await?.triggers.is_empty());
        Ok(())
    }

    /// The same on Postgres, where the edits run in one transaction. `DBM_PG_URL` points elsewhere than the default.
    #[test]
    #[ignore = "needs a local Postgres; creates and drops a scratch database"]
    fn local_postgres_checks_keys_and_triggers() {
        tauri::async_runtime::block_on(async {
            let url =
                std::env::var("DBM_PG_URL").unwrap_or_else(|_| "postgres://postgres@127.0.0.1:5432/postgres".into());
            let open = |url: String| async move {
                Conn::open(Engine::Postgres, &url, None, &Tls::default(), &Options::default(), None).await.unwrap()
            };
            let mut admin = open(url.clone()).await;
            let db = format!("dbm_keys_{}", std::process::id());
            admin.query(&format!("create database {db}")).await.unwrap();
            let mut u = url::Url::parse(&url).unwrap();
            u.set_path(&db);
            let mut c = open(u.to_string()).await;
            let outcome = postgres_keys_scenario(&mut c).await;
            drop(c);
            admin.query(&format!("drop database {db}")).await.unwrap();
            outcome.unwrap();
        });
    }

    async fn postgres_keys_scenario(c: &mut Conn) -> crate::error::Result<()> {
        let create = [
            Change::AddColumn { column: ColumnDef { nullable: false, primary: true, ..col("id", "integer") } },
            Change::AddColumn { column: ColumnDef { nullable: false, ..col("tags", "text[]") } },
            Change::AddCheck { name: "tags_some".into(), expression: "cardinality(tags) > 0".into() },
        ];
        apply(c, "public.t", true, &create).await?;
        let changes = [
            Change::DropCheck { name: "tags_some".into() },
            Change::AddCheck { name: "id_positive".into(), expression: "id > 0".into() },
            Change::SetPrimaryKey { columns: vec!["id".into(), "tags".into()] },
            Change::AddIndex {
                name: "t_tags".into(),
                columns: vec!["tags".into()],
                unique: false,
                method: Some(IndexMethod::Gin),
            },
        ];
        apply(c, "public.t", false, &changes).await?;
        let s = c.structure("public.t").await?;
        assert_eq!(s.checks.iter().map(|k| k.name.as_str()).collect::<Vec<_>>(), ["id_positive"]);
        assert_eq!(
            s.indexes.iter().find(|i| i.primary).map(|i| i.columns.clone()),
            Some(vec!["id".into(), "tags".into()])
        );
        assert_eq!(s.indexes.iter().find(|i| i.name == "t_tags").and_then(|i| i.method.as_deref()), Some("gin"));
        // A failing statement rolls the whole set back: the key and checks stay as they were.
        let bad =
            [Change::DropCheck { name: "id_positive".into() }, Change::SetPrimaryKey { columns: vec!["nope".into()] }];
        assert!(apply(c, "public.t", false, &bad).await.is_err());
        assert_eq!(c.structure("public.t").await?.checks.len(), 1);

        c.query("create function bump() returns trigger language plpgsql as $$ begin new.id := new.id + 1; return new; end $$")
            .await?;
        let add = Change::AddTrigger {
            name: "t_bump".into(),
            timing: TriggerTiming::Before,
            events: vec![TriggerEvent::Insert, TriggerEvent::Update],
            body: "bump()".into(),
        };
        apply(c, "public.t", false, &[add]).await?;
        c.query("insert into t values (1, '{a}')").await?;
        assert_eq!(c.query("select id from t").await?.rows[0][0].as_deref(), Some("2"));
        let tr = &c.structure("public.t").await?.triggers[0];
        assert_eq!(tr.events, ["INSERT", "UPDATE"]);
        assert_eq!((tr.timing.as_deref(), tr.body.as_deref()), (Some("BEFORE"), Some("bump()")));
        apply(c, "public.t", false, &[Change::DropTrigger { name: "t_bump".into() }]).await?;
        assert!(c.structure("public.t").await?.triggers.is_empty());
        Ok(())
    }

    async fn mysql_scenario(c: &mut Conn) -> crate::error::Result<()> {
        let id = ColumnDef { nullable: false, auto_increment: true, primary: true, ..col("id", "int") };
        let owners = [Change::AddColumn { column: id.clone() }, Change::Comment { comment: Some("people".into()) }];
        apply(c, "owners", true, &owners).await?;
        let pets = [
            Change::AddColumn { column: id },
            Change::AddColumn { column: ColumnDef { comment: Some("it's \\ theirs".into()), ..col("owner", "int") } },
            Change::AddColumn {
                column: ColumnDef { default: Some("'Rex'".into()), nullable: false, ..col("name", "varchar(20)") },
            },
            Change::AddForeignKey {
                name: "pets_owner".into(),
                columns: vec!["owner".into()],
                ref_table: "owners".into(),
                ref_columns: vec!["id".into()],
                on_update: Rule::Restrict,
                on_delete: Rule::SetNull,
            },
        ];
        apply(c, "pets", true, &pets).await?;
        c.query("alter table pets add column at timestamp default current_timestamp on update current_timestamp")
            .await?;

        let s = c.structure("pets").await?;
        assert_eq!(s.columns[1].comment.as_deref(), Some("it's \\ theirs"));
        assert_eq!(s.columns[2].default.as_deref(), Some("'Rex'"), "string defaults read back as SQL");
        assert_eq!(s.columns[3].on_update.as_deref(), Some("CURRENT_TIMESTAMP"));
        assert_eq!(
            (s.foreign_keys[0].on_update.as_str(), s.foreign_keys[0].on_delete.as_str()),
            ("RESTRICT", "SET NULL")
        );
        assert_eq!(c.structure("owners").await?.comment.as_deref(), Some("people"));

        // Unchanged columns produce nothing; a renamed one keeps its default and ON UPDATE.
        let at = ColumnDef { default: Some("CURRENT_TIMESTAMP".into()), ..col("changed_at", "timestamp") };
        let same_name = ColumnDef { default: Some("'Rex'".into()), nullable: false, ..col("name", "varchar(20)") };
        let changes = [
            Change::ModifyColumn { from: "name".into(), column: same_name },
            Change::ModifyColumn { from: "at".into(), column: at },
            Change::AddIndex { name: "pets_name".into(), columns: vec!["name".into()], unique: false, method: None },
            Change::DropForeignKey { name: "pets_owner".into() },
        ];
        let sql = c.alter_sql("pets", false, &changes).await?;
        assert_eq!(sql.len(), 3, "{sql:?}");
        c.run_ddl(&sql).await?;
        let s = c.structure("pets").await?;
        assert_eq!(s.columns[3].on_update.as_deref(), Some("CURRENT_TIMESTAMP"));
        assert!(s.foreign_keys.is_empty() && s.indexes.iter().any(|i| i.name == "pets_name"));
        assert!(c.ddl("pets").await?.starts_with("CREATE TABLE `pets`"));

        // MySQL commits as it goes: the first statement stays, and the error says so.
        let partial = ["ALTER TABLE pets ADD COLUMN extra int".to_string(), "ALTER TABLE nope ADD COLUMN x int".into()];
        let err = c.run_ddl(&partial).await.unwrap_err();
        assert!(
            matches!(&err, Error::DdlPartlyApplied { ran, .. } if ran == "ALTER TABLE pets ADD COLUMN extra int"),
            "{err}"
        );
        assert!(c.structure("pets").await?.columns.iter().any(|c| c.name == "extra"));

        c.query("insert into owners values (1)").await?;
        c.query("insert into pets(owner) values (1), (1)").await?;
        c.query("alter table pets add constraint pets_owner foreign key (owner) references owners(id)").await?;
        for op in [
            TableOp::Clone { table: "pets".into(), to: "pets_copy".into(), data: true },
            TableOp::Truncate { table: "owners".into(), cascade: false, restart_identity: false, ignore_fk: true },
            TableOp::ResetAutoIncrement { table: "pets".into() },
            TableOp::Drop { table: "owners".into(), cascade: false, ignore_fk: true },
            TableOp::Rename { table: "pets_copy".into(), to: "pets2".into() },
        ] {
            let sql = c.op_sql(&op).await?;
            c.run_ddl(&sql).await?;
        }
        assert_eq!(c.query("select count(*) from pets2").await?.rows[0][0].as_deref(), Some("2"));
        assert_eq!(c.query("select @@foreign_key_checks").await?.rows[0][0].as_deref(), Some("1"), "restored");
        Ok(())
    }
}
