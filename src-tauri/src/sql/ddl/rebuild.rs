//! SQLite's table rebuild (<https://sqlite.org/lang_altertable.html#otheralter>). SQLite's ALTER TABLE renames, adds
//! and drops columns and nothing more, so a column's type, nullability or default, the column order, foreign keys,
//! checks and the primary key change by creating the table anew from its own CREATE statement with those changes
//! made, copying the rows, dropping the old table and renaming the new one into its place. The CREATE statement is
//! edited through sqlparser, so what the editor doesn't show (COLLATE, UNIQUE, generated columns, WITHOUT ROWID,
//! STRICT) stays as written. The indexes and triggers go back as they were; what ALTER TABLE can do itself (renames,
//! drops, new indexes and triggers) runs after, so SQLite carries it into them. All of it is one transaction, with
//! foreign keys off around it when they were on: dropping the old table must not cascade into other tables.

use super::{
    Change, ColumnDef, check_sql, column_sql, foreign_key_sql, idents, index_sql, not_blank, rename_sql, trigger_sql,
};
use crate::error::{Error, Result};
use crate::sql::db::Dialect;
use crate::sql::structure::{NamedDef, Structure};
use sqlparser::ast::{ColumnOption, CreateTable, DataType, Expr, Ident, ObjectName, Statement, TableConstraint};
use sqlparser::dialect::SQLiteDialect;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::Token;

const D: Dialect = Dialect::Lite;

/// What the rebuild needs from the database besides the structure: the table's CREATE statement, its indexes' and
/// triggers' (name, SQL), and whether foreign keys are being enforced.
pub struct Sources {
    pub create: String,
    pub indexes: Vec<(String, String)>,
    pub triggers: Vec<(String, String)>,
    pub foreign_keys_on: bool,
}

/// Whether `changes` need a rebuild: anything SQLite's ALTER TABLE can't do.
pub fn needed(current: &Structure, changes: &[Change]) -> bool {
    changes.iter().any(|c| match c {
        Change::ModifyColumn { from, column } => current.columns.iter().find(|x| &x.name == from).is_some_and(|cur| {
            !column.data_type.trim().eq_ignore_ascii_case(&cur.data_type)
                || column.nullable != cur.nullable
                || column.auto_increment != cur.auto_increment
                || super::blank_is_none(column.default.as_ref()) != cur.default.as_deref()
        }),
        Change::MoveColumn { .. }
        | Change::AddForeignKey { .. }
        | Change::DropForeignKey { .. }
        | Change::AddCheck { .. }
        | Change::DropCheck { .. }
        | Change::SetPrimaryKey { .. } => true,
        _ => false,
    })
}

fn parse(sql: &str) -> Result<Statement> {
    let mut stmts = Parser::parse_sql(&SQLiteDialect {}, sql).map_err(|e| Error::Rebuild(e.to_string()))?;
    match (stmts.pop(), stmts.is_empty()) {
        (Some(s), true) => Ok(s),
        _ => Err(Error::Invalid("expected one statement")),
    }
}

/// A CREATE TABLE statement, parsed.
fn parse_create(sql: &str) -> Result<CreateTable> {
    match parse(sql)? {
        Statement::CreateTable(ct) => Ok(ct),
        _ => Err(Error::Invalid("the table's definition isn't a CREATE TABLE")),
    }
}

/// A column, a type or a table constraint, parsed from SQL the way they appear inside CREATE TABLE.
fn parse_column(def: &str) -> Result<sqlparser::ast::ColumnDef> {
    parse_create(&format!("CREATE TABLE x ({def})"))?.columns.pop().ok_or(Error::Invalid("not a column definition"))
}

fn parse_type(data_type: &str) -> Result<DataType> {
    Ok(parse_column(&format!("x {data_type}"))?.data_type)
}

fn parse_default(sql: &str) -> Result<Expr> {
    parse_column(&format!("x DEFAULT {sql}"))?
        .options
        .into_iter()
        .find_map(|o| match o.option {
            ColumnOption::Default(e) => Some(e),
            _ => None,
        })
        .ok_or(Error::Invalid("not a default value"))
}

fn parse_constraint(sql: &str) -> Result<TableConstraint> {
    parse_create(&format!("CREATE TABLE x (x, {sql})"))?.constraints.pop().ok_or(Error::Invalid("not a constraint"))
}

/// The table's checks, column ones and table ones in order. An unnamed check is called `check N`, its place among
/// them, so it can still be dropped.
pub(crate) fn checks(create: &str) -> Vec<NamedDef> {
    let Ok(ct) = parse_create(create) else { return vec![] };
    all_checks(&ct)
        .into_iter()
        .enumerate()
        .map(|(i, (name, expr))| NamedDef {
            name: name.map_or_else(|| format!("check {}", i + 1), |n| n.value.clone()),
            definition: format!("CHECK ({expr})"),
        })
        .collect()
}

fn all_checks(ct: &CreateTable) -> Vec<(Option<&Ident>, &Expr)> {
    let in_columns = ct.columns.iter().flat_map(|c| &c.options).filter_map(|o| match &o.option {
        ColumnOption::Check(k) => Some((o.name.as_ref().or(k.name.as_ref()), &*k.expr)),
        _ => None,
    });
    let in_table = ct.constraints.iter().filter_map(|c| match c {
        TableConstraint::Check(k) => Some((k.name.as_ref(), &*k.expr)),
        _ => None,
    });
    in_columns.chain(in_table).collect()
}

/// Drops check `name` (as [`checks`] names it) from the definition.
fn drop_check(ct: &mut CreateTable, name: &str) -> Result<()> {
    let place = checks_named(ct)
        .iter()
        .position(|n| n == name)
        .ok_or(Error::Invalid("that check isn't in the table's definition"))?;
    let mut i = 0;
    let mut hit = |is_check: bool| {
        let here = is_check && i == place;
        i += usize::from(is_check);
        here
    };
    for c in &mut ct.columns {
        c.options.retain(|o| !hit(matches!(o.option, ColumnOption::Check(_))));
    }
    ct.constraints.retain(|c| !hit(matches!(c, TableConstraint::Check(_))));
    Ok(())
}

fn checks_named(ct: &CreateTable) -> Vec<String> {
    all_checks(ct)
        .into_iter()
        .enumerate()
        .map(|(i, (name, _))| name.map_or_else(|| format!("check {}", i + 1), |n| n.value.clone()))
        .collect()
}

/// The column's name in the table as it is now: the rebuild comes before the renames, which run after it.
fn original<'a>(changes: &'a [Change], name: &'a str) -> &'a str {
    changes
        .iter()
        .find_map(|c| match c {
            Change::ModifyColumn { from, column } if column.name.trim() == name => Some(from.as_str()),
            _ => None,
        })
        .unwrap_or(name)
}

fn originals(changes: &[Change], names: &[String]) -> Vec<String> {
    names.iter().map(|n| original(changes, n).to_string()).collect()
}

/// Whether `option` makes the column (part of) the primary key, or is the AUTOINCREMENT that goes with it.
fn primary_key_option(option: &ColumnOption) -> bool {
    match option {
        ColumnOption::PrimaryKey(_) => true,
        ColumnOption::DialectSpecific(tokens) => {
            tokens.iter().any(|t| matches!(t, Token::Word(w) if w.value.eq_ignore_ascii_case("AUTOINCREMENT")))
        }
        _ => false,
    }
}

fn generated(c: &sqlparser::ast::ColumnDef) -> bool {
    c.options.iter().any(|o| matches!(o.option, ColumnOption::Generated { .. }))
}

/// The table's definition with the changes made, and what's left for after the rebuild.
struct Plan<'a> {
    ct: CreateTable,
    /// What ALTER TABLE does itself, run once the rebuilt table is in place: renames, drops, new indexes and triggers.
    after_rebuild: Vec<String>,
    rename: Option<String>,
    dropped_indexes: Vec<&'a str>,
    dropped_triggers: Vec<&'a str>,
}

fn plan<'a>(t: &str, current: &Structure, changes: &'a [Change], create: &str) -> Result<Plan<'a>> {
    let mut p = Plan {
        ct: parse_create(create)?,
        after_rebuild: vec![],
        rename: None,
        dropped_indexes: vec![],
        dropped_triggers: vec![],
    };
    let ct = &mut p.ct;
    for change in changes {
        match change {
            Change::AddColumn { column } => {
                if column.primary {
                    return Err(Error::Invalid("set the primary key with the Key column, not on a new column"));
                }
                ct.columns.push(parse_column(&column_sql(D, column, None)?)?);
            }
            Change::DropColumn { name } => {
                p.after_rebuild.push(format!("ALTER TABLE {t} DROP COLUMN {}", D.ident(name)));
            }
            Change::ModifyColumn { from, column } => modify_column(ct, current, t, from, column, &mut p.after_rebuild)?,
            Change::MoveColumn { name, after } => {
                move_column(ct, original(changes, name), after.as_deref().map(|a| original(changes, a)))?;
            }
            Change::AddForeignKey { name, columns, ref_table, ref_columns, on_update, on_delete } => {
                if columns.is_empty() || columns.len() != ref_columns.len() {
                    return Err(Error::Invalid("a foreign key needs as many referenced columns as columns"));
                }
                let fk = foreign_key_sql(D, name, &originals(changes, columns), ref_table, ref_columns);
                ct.constraints.push(parse_constraint(&format!(
                    "{fk} ON UPDATE {} ON DELETE {}",
                    on_update.sql(),
                    on_delete.sql()
                ))?);
            }
            Change::DropForeignKey { name } => drop_foreign_key(ct, current, name)?,
            Change::AddCheck { name, expression } => {
                ct.constraints.push(parse_constraint(&check_sql(D, name, expression)?)?);
            }
            Change::DropCheck { name } => drop_check(ct, name)?,
            Change::SetPrimaryKey { columns } => set_primary_key(ct, &originals(changes, columns))?,
            Change::AddIndex { name, columns, unique, method } => {
                p.after_rebuild.push(index_sql(D, t, name, columns, *unique, *method)?);
            }
            Change::DropIndex { name } => p.dropped_indexes.push(name.as_str()),
            Change::AddTrigger { name, timing, events, body } => {
                p.after_rebuild.push(trigger_sql(D, t, t, name, *timing, events, body)?); // the plain name only matters to SQL Server
            }
            Change::DropTrigger { name } => p.dropped_triggers.push(name.as_str()),
            Change::Rename { to } => p.rename = Some(rename_sql(D, t, not_blank(to, "the table needs a name")?)),
            Change::Comment { .. } => return Err(Error::Invalid("SQLite tables have no comments")),
            Change::Engine { .. } | Change::Collation { .. } => {
                return Err(Error::Invalid("only MySQL tables have an engine and a collation"));
            }
        }
    }
    Ok(p)
}

fn move_column(ct: &mut CreateTable, name: &str, after: Option<&str>) -> Result<()> {
    let from = ct
        .columns
        .iter()
        .position(|c| c.name.value == name)
        .ok_or(Error::Invalid("a moved column no longer exists"))?;
    let moved = ct.columns.remove(from);
    let at = match after {
        None => 0,
        Some(a) => {
            1 + ct
                .columns
                .iter()
                .position(|c| c.name.value == a)
                .ok_or(Error::Invalid("a moved column's neighbour no longer exists"))?
        }
    };
    ct.columns.insert(at, moved);
    Ok(())
}

/// The key on `columns` (none when empty) in place of the one there was, whether a column's or the table's.
fn set_primary_key(ct: &mut CreateTable, columns: &[String]) -> Result<()> {
    for c in &mut ct.columns {
        c.options.retain(|o| !primary_key_option(&o.option));
    }
    ct.constraints.retain(|c| !matches!(c, TableConstraint::PrimaryKey(_)));
    if !columns.is_empty() {
        ct.constraints.insert(0, parse_constraint(&format!("PRIMARY KEY ({})", idents(D, columns)))?);
    }
    Ok(())
}

/// The statements that make `changes` to `table` by rebuilding it; see the module comment.
pub fn rebuild_sql(table: &str, current: &Structure, changes: &[Change], src: &Sources) -> Result<Vec<String>> {
    let t = D.ident(table);
    let before: Vec<String> = parse_create(&src.create)?.columns.iter().map(|c| c.name.value.clone()).collect();
    let Plan { mut ct, after_rebuild, rename, dropped_indexes, dropped_triggers } =
        plan(&t, current, changes, &src.create)?;
    if let Some(name) = dropped_indexes.iter().find(|n| !src.indexes.iter().any(|(i, _)| i == *n)) {
        return Err(Error::Invalid(if name.starts_with("sqlite_autoindex_") {
            "that index belongs to a UNIQUE or PRIMARY KEY constraint; change the constraint instead"
        } else {
            "a dropped index no longer exists; reopen the structure"
        }));
    }
    // The rows go over column by column; new columns take their defaults, generated ones are computed again.
    let copied: Vec<String> = ct
        .columns
        .iter()
        .filter(|c| before.contains(&c.name.value) && !generated(c))
        .map(|c| c.name.value.clone())
        .collect();
    let temp = format!("{table}__rebuild");
    ct.name = ObjectName::from(vec![Ident::with_quote('"', temp.clone())]);
    let temp = D.ident(&temp);
    let cols = idents(D, &copied);
    let mut out = Vec::new();
    if src.foreign_keys_on {
        out.push("PRAGMA foreign_keys = OFF".to_string());
    }
    out.push(Statement::CreateTable(ct).to_string());
    out.push(format!("INSERT INTO {temp} ({cols}) SELECT {cols} FROM {t}"));
    out.push(format!("DROP TABLE {t}"));
    // Legacy rename: the views that name the table aren't checked against it while it's gone, nor rewritten.
    out.push("PRAGMA legacy_alter_table = ON".to_string());
    out.push(format!("ALTER TABLE {temp} RENAME TO {t}"));
    out.push("PRAGMA legacy_alter_table = OFF".to_string());
    out.extend(src.indexes.iter().filter(|(n, _)| !dropped_indexes.contains(&n.as_str())).map(|(_, sql)| sql.clone()));
    out.extend(
        src.triggers.iter().filter(|(n, _)| !dropped_triggers.contains(&n.as_str())).map(|(_, sql)| sql.clone()),
    );
    out.extend(after_rebuild);
    out.extend(rename);
    if src.foreign_keys_on {
        out.push("PRAGMA foreign_keys = ON".to_string());
    }
    Ok(out)
}

/// A changed column: its type, nullability and default change in the definition; a new name (and nothing else
/// SQLite's ALTER TABLE can do) is left to run after the rebuild.
fn modify_column(
    ct: &mut CreateTable,
    current: &Structure,
    t: &str,
    from: &str,
    want: &ColumnDef,
    after_rebuild: &mut Vec<String>,
) -> Result<()> {
    let cur = current
        .columns
        .iter()
        .find(|c| c.name == from)
        .ok_or(Error::Invalid("a changed column no longer exists; reopen the structure"))?;
    if want.auto_increment != cur.auto_increment {
        return Err(Error::Invalid("SQLite numbers an INTEGER PRIMARY KEY by itself; that can't be switched here"));
    }
    if super::blank_is_none(want.comment.as_ref()).is_some() {
        return Err(Error::Invalid("SQLite columns have no comments"));
    }
    let col = ct
        .columns
        .iter_mut()
        .find(|c| c.name.value == from)
        .ok_or(Error::Invalid("a changed column isn't in the table's definition"))?;
    let data_type = not_blank(&want.data_type, "every column needs a type")?;
    if !data_type.eq_ignore_ascii_case(&cur.data_type) {
        col.data_type = parse_type(data_type)?;
    }
    if want.nullable != cur.nullable {
        col.options.retain(|o| !matches!(o.option, ColumnOption::Null | ColumnOption::NotNull));
        if !want.nullable {
            col.options.insert(0, parse_column("x NOT NULL")?.options.remove(0));
        }
    }
    let default = super::blank_is_none(want.default.as_ref());
    if default != cur.default.as_deref() {
        col.options.retain(|o| !matches!(o.option, ColumnOption::Default(_)));
        if let Some(v) = default {
            col.options
                .push(sqlparser::ast::ColumnOptionDef { name: None, option: ColumnOption::Default(parse_default(v)?) });
        }
    }
    let name = not_blank(&want.name, "every column needs a name")?;
    if name != from {
        after_rebuild.push(format!("ALTER TABLE {t} RENAME COLUMN {} TO {}", D.ident(from), D.ident(name)));
    }
    Ok(())
}

/// Drops the foreign key the structure calls `name` (SQLite's have none of their own) by what it is: its columns and
/// the table it references, as a table constraint or a column's REFERENCES.
fn drop_foreign_key(ct: &mut CreateTable, current: &Structure, name: &str) -> Result<()> {
    let fk = current
        .foreign_keys
        .iter()
        .find(|f| f.name == name)
        .ok_or(Error::Invalid("that foreign key no longer exists"))?;
    let names = |ids: &[Ident]| ids.iter().map(|i| i.value.clone()).collect::<Vec<_>>();
    let refers =
        |table: &ObjectName| table.0.last().and_then(|p| p.as_ident()).is_some_and(|i| i.value == fk.ref_table);
    let before = ct.constraints.len();
    ct.constraints.retain(
        |c| !matches!(c, TableConstraint::ForeignKey(k) if names(&k.columns) == fk.columns && refers(&k.foreign_table)),
    );
    if ct.constraints.len() < before {
        return Ok(());
    }
    if let [column] = fk.columns.as_slice()
        && let Some(c) = ct.columns.iter_mut().find(|c| &c.name.value == column)
    {
        let before = c.options.len();
        c.options.retain(|o| !matches!(&o.option, ColumnOption::ForeignKey(k) if refers(&k.foreign_table)));
        if c.options.len() < before {
            return Ok(());
        }
    }
    Err(Error::Invalid("that foreign key isn't in the table's definition as written"))
}
