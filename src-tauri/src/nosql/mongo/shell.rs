//! mongosh syntax, read by running it in QuickJS (`shell.js`): `db.orders.find({…}).sort({…})` and the like record
//! the operations a script asks for, which [`super`] then runs. The sandbox has no network, file or app access, and
//! stops after a moment, so a script can only describe operations, never perform them.

use crate::error::{Error, Result};
use mongodb::bson::Bson;
use rquickjs::{CatchResultExt, Context, Runtime};
use serde::Deserialize;
use serde_json::Value;
use std::time::{Duration, Instant};

const PRELUDE: &str = include_str!("shell.js");
const MEMORY: usize = 32 * 1024 * 1024;
const STACK: usize = 1024 * 1024;
const TIME_LIMIT: Duration = Duration::from_secs(2);
/// A script, or a value typed in, longer than this isn't something to type.
const MAX_SCRIPT: usize = 4 * 1024 * 1024;

/// One operation a script asked for. Which fields it has depends on `op` (shell.js names them).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Operation {
    pub op: String,
    #[serde(default)]
    pub db: Option<String>,
    #[serde(default)]
    pub collection: Option<String>,
    #[serde(default)]
    pub filter: Option<Value>,
    #[serde(default)]
    pub projection: Option<Value>,
    #[serde(default)]
    pub sort: Option<Value>,
    #[serde(default)]
    pub skip: Option<f64>,
    #[serde(default)]
    pub limit: Option<f64>,
    #[serde(default)]
    pub explain: Option<bool>,
    #[serde(default)]
    pub pipeline: Option<Vec<Value>>,
    #[serde(default)]
    pub options: Option<Value>,
    #[serde(default)]
    pub field: Option<String>,
    #[serde(default)]
    pub document: Option<Value>,
    #[serde(default)]
    pub documents: Option<Vec<Value>>,
    #[serde(default)]
    pub update: Option<Value>,
    #[serde(default)]
    pub replacement: Option<Value>,
    #[serde(default)]
    pub keys: Option<Value>,
    #[serde(default)]
    pub index: Option<Value>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub command: Option<Value>,
    #[serde(default)]
    pub value: Option<Value>,
}

/// mongosh's `show …` and `use …` lines, which aren't JavaScript, as calls into shell.js.
fn shell_commands(script: &str) -> String {
    script
        .lines()
        .map(|line| {
            let words: Vec<&str> = line.trim().trim_end_matches(';').split_whitespace().collect();
            let word_ok =
                |w: &str| !w.is_empty() && w.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-.$".contains(&b));
            match words.as_slice() {
                ["show", what] if word_ok(what) => format!("__show({});", serde_json::json!(what)),
                ["use", name] if word_ok(name) => format!("__use({});", serde_json::json!(name)),
                _ => line.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Runs `body` after shell.js, with `db` on `database` and `__input` holding `input`; returns what it gives as text.
fn eval(database: &str, input: &str, body: &str) -> Result<String> {
    if input.len() > MAX_SCRIPT {
        return Err(Error::Invalid("the script is too long"));
    }
    let failed = |e: rquickjs::Error| Error::Mongo(e.to_string());
    let runtime = Runtime::new().map_err(failed)?;
    runtime.set_memory_limit(MEMORY);
    runtime.set_max_stack_size(STACK);
    let deadline = Instant::now() + TIME_LIMIT;
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() > deadline)));
    let context = Context::full(&runtime).map_err(failed)?;
    let out = context.with(|ctx| -> std::result::Result<String, String> {
        let globals = ctx.globals();
        globals.set("__database", database).map_err(|e| e.to_string())?;
        globals.set("__input", input).map_err(|e| e.to_string())?;
        ctx.eval::<(), _>(PRELUDE).catch(&ctx).map_err(|e| e.to_string())?;
        ctx.eval::<String, _>(body).catch(&ctx).map_err(|e| e.to_string())
    });
    out.map_err(|e| {
        if Instant::now() > deadline {
            Error::Mongo(format!("the script ran for over {} seconds and was stopped", TIME_LIMIT.as_secs()))
        } else {
            Error::Mongo(script_error(&e))
        }
    })
}

/// QuickJS's error without its stack, but with where in the script it happened: `message (line 2, column 7)`.
fn script_error(text: &str) -> String {
    let mut lines = text.trim().lines();
    let message = lines.next().unwrap_or_default().trim_start_matches("Error: ").to_string();
    let at = lines.find_map(|l| l.trim().strip_prefix("at <input>:")).and_then(|at| at.split_once(':'));
    match at {
        Some((line, column)) => format!("{message} (line {line}, column {column})"),
        None => message,
    }
}

/// The operations `script` asks for, in order, with `db` on `database`. A script that only computes a value (no
/// operation) gives one `value` operation holding it.
pub fn operations(script: &str, database: &str) -> Result<Vec<Operation>> {
    let body = "(() => { const v = (0, eval)(__input); \
                if (!__ops.length && v !== undefined && !(v instanceof Op) && !(v instanceof Collection)) \
                __ops.push({ op: 'value', value: v }); \
                return JSON.stringify(__ops, __ext); })()";
    let json = eval(database, &shell_commands(script), body)?;
    Ok(serde_json::from_str(&json)?)
}

/// Each expression's value as BSON: `42`, `"text"`, `ObjectId("…")`, `{ a: [1, 2] }`, `ISODate("…")`.
pub fn values(expressions: &[&str]) -> Result<Vec<Bson>> {
    let input = serde_json::to_string(expressions)?;
    let body = "JSON.stringify(JSON.parse(__input).map((e) => { const v = (0, eval)('(' + e + '\\n)'); \
                if (v === undefined) throw new Error('nothing to save: ' + e); return v; }), __ext)";
    let json = eval("test", &input, body)?;
    let list: Vec<Value> = serde_json::from_str(&json)?;
    if list.len() != expressions.len() {
        return Err(Error::Mongo("a value didn't read as one value".into()));
    }
    list.into_iter().map(to_bson).collect()
}

/// Extended JSON (what shell.js writes) as BSON.
pub fn to_bson(v: Value) -> Result<Bson> {
    Bson::try_from(v).map_err(|e| Error::Mongo(format!("not a BSON value: {e}")))
}

#[cfg(test)]
mod tests {
    use super::{operations, shell_commands, values};
    use mongodb::bson::{Bson, doc, oid::ObjectId};
    use serde_json::json;

    #[test]
    fn operations_should_record_a_find_with_its_chain_in_order() {
        let ops = operations(
            "db.orders.find({ total: { $gt: 5 } }, { _id: 0 }).sort({ total: -1 }).skip(10).limit(5);\n\
             db.getCollection('order items').countDocuments({})",
            "shop",
        )
        .unwrap();
        assert_eq!(ops.len(), 2);
        assert_eq!(
            (ops[0].op.as_str(), ops[0].collection.as_deref(), ops[0].db.as_deref()),
            ("find", Some("orders"), Some("shop"))
        );
        assert_eq!(ops[0].filter, Some(json!({ "total": { "$gt": 5 } })));
        assert_eq!(ops[0].projection, Some(json!({ "_id": 0 })));
        assert_eq!(ops[0].sort, Some(json!({ "total": -1 })));
        assert_eq!((ops[0].skip, ops[0].limit), (Some(10.0), Some(5.0)));
        assert_eq!((ops[1].op.as_str(), ops[1].collection.as_deref()), ("countDocuments", Some("order items")));
    }

    #[test]
    fn operations_should_keep_bson_types_as_extended_json() {
        let ops = operations(
            "db.t.insertOne({ _id: ObjectId('64b7f0c2a1b2c3d4e5f60718'), at: ISODate('2024-01-02T03:04:05Z'), \
             n: NumberLong('9007199254740993'), re: /ab+c/i, d: NumberDecimal('1.10') })",
            "test",
        )
        .unwrap();
        let doc = super::to_bson(ops[0].document.clone().unwrap()).unwrap();
        let Bson::Document(d) = doc else { panic!("not a document") };
        assert_eq!(d.get_object_id("_id").unwrap(), ObjectId::parse_str("64b7f0c2a1b2c3d4e5f60718").unwrap());
        assert!(matches!(d.get("at"), Some(Bson::DateTime(_))));
        assert_eq!(d.get_i64("n").unwrap(), 9_007_199_254_740_993);
        assert!(matches!(d.get("re"), Some(Bson::RegularExpression(r)) if r.pattern == "ab+c" && r.options == "i"));
        assert!(matches!(d.get("d"), Some(Bson::Decimal128(_))));
    }

    #[test]
    fn operations_should_read_show_and_use_lines() {
        assert_eq!(
            shell_commands("show dbs\n  use shop;\nshow\nuse a b"),
            "__show(\"dbs\");\n__use(\"shop\");\nshow\nuse a b"
        );
        let ops = operations("use shop\ndb.items.find()\nshow collections", "test").unwrap();
        let seen: Vec<_> = ops.iter().map(|o| (o.op.as_str(), o.db.as_deref())).collect();
        assert_eq!(seen, [("use", Some("shop")), ("find", Some("shop")), ("listCollections", Some("shop"))]);
    }

    #[test]
    fn operations_should_turn_a_plain_expression_into_a_value() {
        let ops = operations("1 + 1", "test").unwrap();
        assert_eq!((ops[0].op.as_str(), ops[0].value.clone()), ("value", Some(json!(2))));
    }

    #[test]
    fn operations_should_report_script_errors_and_stop_runaway_scripts() {
        let e = operations("db.t.find(", "test").unwrap_err().to_string();
        assert!(e.contains("unexpected token") && e.ends_with("(line 1, column 11)"), "{e}");
        assert!(operations("ObjectId('nope')", "test").unwrap_err().to_string().contains("24 hexadecimal"));
        assert!(operations("while (true) {}", "test").unwrap_err().to_string().contains("stopped"));
    }

    #[test]
    fn values_should_read_each_expression_as_one_bson_value() {
        let v = values(&[
            "42",
            "'it''s'".replace("''", "\\'").as_str(),
            "{ a: [1, 'x'] }",
            "ObjectId('64b7f0c2a1b2c3d4e5f60718')",
        ])
        .unwrap();
        assert_eq!(v[0], Bson::Int32(42));
        assert_eq!(v[1], Bson::String("it's".into()));
        assert_eq!(v[2], Bson::Document(doc! { "a": [1, "x"] }));
        assert!(matches!(v[3], Bson::ObjectId(_)));
        assert!(values(&["}"]).is_err());
    }
}
